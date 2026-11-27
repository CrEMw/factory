-- Baseline schema of the hub database, sqlx version 1000 (step 2.12b of the factory plan): the schema that the
-- migration chain services/hub/migrations (0001..0029) leaves on an empty database, in one file. A new installation
-- applies only this file; an existing database adopts it (`hub migrate --adopt`, step 2.12c) once its catalog equals
-- this file.
--
-- The chain and this file give the same catalog: tools/test-pg-schema.sh applies both to empty databases and
-- compares tools/pg-schema.sql byte for byte (function bodies, signatures and attributes, views, comments, tables,
-- columns, defaults, constraints, indexes, triggers, grants, default privileges, sequences, extensions). Function
-- and view texts here are canonical: 0028_canonical_bodies.sql gave every function, view and comment its text from this
-- file, and every later migration that changes one carries its new text from here verbatim (sh tools/canonical-bodies.sh
-- prints it); the pg-schema part of tools/check.sh fails when the last text of an object in the migrations from 0028 on
-- differs from this file. A schema change is a new migration plus the same change here.
--
-- Layout: pg_dump --schema-only order (schemas, functions, tables and sequences, views, defaults, constraints,
-- indexes, triggers, foreign keys, grants, default privileges), without owners: every object belongs to the role
-- that applies the file (the migration role of the installation). Names are schema-qualified, as in pg_dump; the
-- search path stays as it is, because sqlx writes its journal (unqualified _sqlx_migrations) in the same transaction.
-- Function bodies are not checked at creation: a function may come before the table it reads.
--
-- Roles: hub (tables, sequences and functions of public, graph, plan, mailbox; no DDL, no auth) and hub_web (tables
-- of auth). In a cluster the database operator creates them with passwords before the migrations; the migration
-- role has no CREATEROLE, and a missing role fails the migration. Locally and in tests a superuser applies the
-- file and creates them without login.

SET LOCAL check_function_bodies = false;

DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'hub') THEN
        CREATE ROLE hub NOLOGIN;
    END IF;
    IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'hub_web') THEN
        CREATE ROLE hub_web NOLOGIN;
    END IF;
END;
$$;

CREATE SCHEMA auth;

CREATE SCHEMA graph;

CREATE SCHEMA mailbox;

CREATE SCHEMA plan;

CREATE FUNCTION graph.attr_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    -- Through jsonb: a person has the key handle, a component slug, and a direct NEW.slug does not parse on a person row.
    v_key text := coalesce(to_jsonb(NEW) ->> 'handle', to_jsonb(NEW) ->> 'slug');
BEGIN
    IF nullif(current_setting('hub.actor', true), '') IS NULL
       OR nullif(current_setting('hub.actor_kind', true), '') IS NULL THEN
        RAISE EXCEPTION 'hub.actor and hub.actor_kind are required' USING ERRCODE = 'GRACT';
    END IF;
    IF TG_OP = 'UPDATE' THEN
        IF (NEW.node_id, NEW.account, NEW.kind) IS DISTINCT FROM (OLD.node_id, OLD.account, OLD.kind)
           OR v_key IS DISTINCT FROM coalesce(to_jsonb(OLD) ->> 'handle', to_jsonb(OLD) ->> 'slug') THEN
            RAISE EXCEPTION '%.% identity is immutable', TG_TABLE_SCHEMA, TG_TABLE_NAME USING ERRCODE = 'GRFRB';
        END IF;
    ELSIF NOT EXISTS (
        SELECT 1 FROM graph.node n
        WHERE n.id = NEW.node_id AND n.account = NEW.account AND n.kind = NEW.kind AND n.key = v_key
    ) THEN
        RAISE EXCEPTION '%.% key % does not match its node', TG_TABLE_SCHEMA, TG_TABLE_NAME, v_key
            USING ERRCODE = 'GRKEY';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION graph.document_node() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_id bigint;
BEGIN
    INSERT INTO graph.node(kind, key) VALUES ('document', NEW.repo || ':' || NEW.path)
    ON CONFLICT (account, kind, key) DO NOTHING
    RETURNING id INTO v_id;
    IF v_id IS NULL THEN
        SELECT id INTO v_id FROM graph.node
        WHERE account = public.instance_account() AND kind = 'document' AND key = NEW.repo || ':' || NEW.path;
    END IF;
    INSERT INTO graph.document(node_id, repo, path) VALUES (v_id, NEW.repo, NEW.path)
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE FUNCTION graph.edge_check() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_actor text := nullif(current_setting('hub.actor', true), '');
    v_kind text := nullif(current_setting('hub.actor_kind', true), '');
    v_shape graph.rel_shape;
    v_check_cycle boolean := false;
    v_rels text[];
    v_back bigint[];
    v_path bigint[];
    v_cur bigint;
    v_next bigint;
BEGIN
    IF v_actor IS NULL OR v_kind IS NULL THEN
        RAISE EXCEPTION 'hub.actor and hub.actor_kind are required' USING ERRCODE = 'GRACT';
    END IF;
    IF NEW.src = NEW.dst THEN
        RAISE EXCEPTION 'self_link: an edge joins two different nodes' USING ERRCODE = 'GRSLF';
    END IF;
    SELECT * INTO v_shape FROM graph.rel_shape s
    WHERE s.account = NEW.account AND s.rel = NEW.rel AND s.src_kind = NEW.src_kind AND s.dst_kind = NEW.dst_kind;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'unknown edge shape: % % %', NEW.src_kind, NEW.rel, NEW.dst_kind USING ERRCODE = 'GRSHP';
    END IF;

    IF TG_OP = 'INSERT' THEN
        IF NEW.valid_to IS NOT NULL OR NEW.close_reason IS NOT NULL OR NEW.closed_by IS NOT NULL THEN
            RAISE EXCEPTION 'a new edge is live' USING ERRCODE = 'GRFRB';
        END IF;
        NEW.created_at := now();
        NEW.created_by := v_actor;
        -- A relation that a human confirms: from an agent or a service only a proposal.
        IF NEW.state = 'confirmed' AND v_shape.needs_confirm AND v_kind NOT IN ('human', 'seed') THEN
            NEW.state := 'proposed';
        END IF;
        IF NEW.state = 'confirmed' THEN
            NEW.confirmed_at := now();
            NEW.confirmed_by := v_actor;
            NEW.confirmed_kind := v_kind;
            v_check_cycle := v_shape.acyclic;
        ELSE
            NEW.confirmed_at := NULL;
            NEW.confirmed_by := NULL;
            NEW.confirmed_kind := NULL;
        END IF;
        -- evidence: links only. A flat object, without quote, strings up to 100 characters.
        IF NEW.evidence IS NOT NULL AND (
            NEW.evidence ? 'quote'
            OR (SELECT count(*) FROM jsonb_object_keys(NEW.evidence)) > 20
            OR EXISTS (SELECT 1 FROM jsonb_each(NEW.evidence) e
                       WHERE jsonb_typeof(e.value) NOT IN ('string', 'number', 'boolean', 'null')
                          OR (jsonb_typeof(e.value) = 'string' AND length(e.value #>> '{}') > 100))) THEN
            RAISE EXCEPTION 'evidence holds references only: flat object, no quote, strings up to 100 chars'
                USING ERRCODE = 'GREVD';
        END IF;
    ELSE
        IF NEW.state = 'confirmed' AND OLD.state = 'proposed' THEN
            IF v_kind <> 'human' THEN
                RAISE EXCEPTION 'confirm_requires_human' USING ERRCODE = 'GRHUM';
            END IF;
            NEW.confirmed_at := now();
            NEW.confirmed_by := v_actor;
            NEW.confirmed_kind := v_kind;
            v_check_cycle := v_shape.acyclic AND NEW.valid_to IS NULL;
        END IF;
        IF NEW.valid_to IS NOT NULL AND OLD.valid_to IS NULL THEN
            -- What a human confirmed (and anything confirmed with needs_confirm) is not closed by an agent or a service:
            -- otherwise an agent would close the part_of parent and set its own. Exception: retiring the node.
            IF OLD.state = 'confirmed' AND (OLD.confirmed_kind = 'human' OR v_shape.needs_confirm)
               AND v_kind <> 'human'
               AND NOT (NEW.close_reason = 'node_retired' AND EXISTS (
                   SELECT 1 FROM graph.node n WHERE n.id IN (NEW.src, NEW.dst) AND n.retired_at IS NOT NULL)) THEN
                RAISE EXCEPTION 'close_requires_human' USING ERRCODE = 'GRHCL';
            END IF;
            NEW.valid_to := now();
            NEW.closed_by := v_actor;
        END IF;
    END IF;

    IF NEW.valid_to IS NULL AND (TG_OP = 'INSERT' OR NEW.state IS DISTINCT FROM OLD.state) AND EXISTS (
        SELECT 1 FROM graph.node n WHERE n.id IN (NEW.src, NEW.dst) AND n.retired_at IS NOT NULL
    ) THEN
        RAISE EXCEPTION 'node is retired' USING ERRCODE = 'GRRET';
    END IF;

    IF v_check_cycle THEN
        -- The lock serializes cycle checks: two parallel inserts do not close a cycle together. This holds only
        -- in READ COMMITTED, where the query after the lock sees the edge committed by the first transaction. In REPEATABLE
        -- READ and SERIALIZABLE the snapshot is older than that commit, and the cycle would close silently: such an insert is refused.
        IF current_setting('transaction_isolation') <> 'read committed' THEN
            RAISE EXCEPTION 'cycle check needs READ COMMITTED, got %', current_setting('transaction_isolation')
                USING ERRCODE = 'GRISO';
        END IF;
        PERFORM pg_advisory_xact_lock(193700, 109);
        v_rels := CASE WHEN NEW.rel IN ('blocks', 'part_of') THEN ARRAY['blocks', 'part_of'] ELSE ARRAY[NEW.rel] END;
        -- Core: UNION over a single n collapses nodes, the pass is O(V+E). Nodes from which src is reachable.
        WITH RECURSIVE back(n) AS (
            SELECT NEW.src
            UNION
            SELECT e.src FROM back b
            JOIN graph.edge e ON e.dst = b.n AND e.account = NEW.account AND e.rel = ANY (v_rels)
             AND e.valid_to IS NULL AND e.state = 'confirmed'
        )
        SELECT array_agg(n) INTO v_back FROM back;
        IF NEW.dst = ANY (v_back) THEN
            -- Path dst -> src: the subgraph of confirmed edges is acyclic, so a step to any node from which
            -- src is reachable brings it closer and does not loop.
            v_path := ARRAY[NEW.src, NEW.dst];
            v_cur := NEW.dst;
            WHILE v_cur <> NEW.src AND cardinality(v_path) < 200 LOOP
                SELECT e.dst INTO v_next FROM graph.edge e
                WHERE e.src = v_cur AND e.account = NEW.account AND e.rel = ANY (v_rels)
                  AND e.valid_to IS NULL AND e.state = 'confirmed' AND e.dst = ANY (v_back)
                ORDER BY (e.dst = NEW.src) DESC, e.dst
                LIMIT 1;
                EXIT WHEN v_next IS NULL;
                v_path := v_path || v_next;
                v_cur := v_next;
            END LOOP;
            RAISE EXCEPTION 'cycle' USING ERRCODE = 'GRCYC',
                DETAIL = (SELECT jsonb_agg(graph.node_label(p.id) ORDER BY p.ord)::text
                          FROM unnest(v_path) WITH ORDINALITY AS p(id, ord));
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION graph.edge_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    IF (NEW.id, NEW.account, NEW.src, NEW.src_kind, NEW.rel, NEW.dst, NEW.dst_kind, NEW.evidence,
        NEW.created_at, NEW.created_by)
       IS DISTINCT FROM (OLD.id, OLD.account, OLD.src, OLD.src_kind, OLD.rel, OLD.dst, OLD.dst_kind, OLD.evidence,
        OLD.created_at, OLD.created_by) THEN
        RAISE EXCEPTION 'edge identity is immutable' USING ERRCODE = 'GRFRB';
    END IF;
    IF OLD.valid_to IS NOT NULL THEN
        RAISE EXCEPTION 'edge % is closed', OLD.id USING ERRCODE = 'GRFRB';
    END IF;
    IF OLD.state = 'confirmed' AND NEW.state <> 'confirmed' THEN
        RAISE EXCEPTION 'edge % is confirmed: close it instead', OLD.id USING ERRCODE = 'GRFRB';
    END IF;
    IF NEW.state = OLD.state AND (NEW.confirmed_at, NEW.confirmed_by, NEW.confirmed_kind)
       IS DISTINCT FROM (OLD.confirmed_at, OLD.confirmed_by, OLD.confirmed_kind) THEN
        RAISE EXCEPTION 'confirmation fields change only with the state' USING ERRCODE = 'GRFRB';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION graph.edge_supersede() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    IF NEW.state = 'confirmed' AND NEW.valid_to IS NULL
       AND (TG_OP = 'INSERT' OR OLD.state = 'proposed') THEN
        UPDATE graph.edge e SET valid_to = now(), close_reason = 'superseded'
        WHERE e.account = NEW.account AND e.src = NEW.src AND e.rel = NEW.rel
          AND e.state = 'confirmed' AND e.valid_to IS NULL AND e.dst <> NEW.dst
          AND (TG_OP = 'INSERT' OR e.id <> NEW.id);
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION graph.forbid_delete() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    RAISE EXCEPTION 'delete forbidden on %.%: retire instead', TG_TABLE_SCHEMA, TG_TABLE_NAME USING ERRCODE = 'GRFRB';
END;
$$;

CREATE FUNCTION graph.forbid_update() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    RAISE EXCEPTION 'update forbidden on %.%', TG_TABLE_SCHEMA, TG_TABLE_NAME USING ERRCODE = 'GRFRB';
END;
$$;

CREATE FUNCTION graph.import_components(p_components jsonb) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    r jsonb;
    v_id bigint;
    v_project bigint;
    v_old graph.component;
    v_slug text;
    v_title text;
    v_description text;
    v_tags text[];
    v_new boolean;
    v_n int;
    v_created int := 0;
    v_updated int := 0;
    v_unchanged int := 0;
    v_projects int := 0;
    v_tickets int := 0;
    v_account text := public.instance_account();
BEGIN
    IF jsonb_typeof(p_components) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'import_components: a JSON array expected' USING ERRCODE = '22023';
    END IF;
    PERFORM set_config('hub.actor', 'seed:components', true), set_config('hub.actor_kind', 'seed', true);
    PERFORM pg_advisory_xact_lock(193700, 157);
    FOR r IN SELECT value FROM jsonb_array_elements(p_components) LOOP
        v_slug := r ->> 'slug';
        v_title := r ->> 'title';
        v_description := nullif(r ->> 'description', '');
        v_tags := ARRAY(SELECT jsonb_array_elements_text(coalesce(r -> 'tags', '[]'::jsonb)));
        INSERT INTO graph.node(kind, key) VALUES ('component', v_slug)
        ON CONFLICT (account, kind, key) DO NOTHING
        RETURNING id INTO v_id;
        v_new := v_id IS NOT NULL;
        IF v_id IS NULL THEN
            SELECT id INTO v_id FROM graph.node WHERE account = v_account AND kind = 'component' AND key = v_slug;
        END IF;
        SELECT * INTO v_old FROM graph.component WHERE node_id = v_id;
        IF NOT FOUND THEN
            INSERT INTO graph.component(node_id, slug, title, description) VALUES (v_id, v_slug, v_title, v_description);
            v_created := v_created + 1;
        ELSIF (v_old.title, v_old.description) IS DISTINCT FROM (v_title, v_description) THEN
            UPDATE graph.component SET title = v_title, description = v_description WHERE node_id = v_id;
            v_updated := v_updated + 1;
        ELSE
            v_unchanged := v_unchanged + 1;
        END IF;
        CONTINUE WHEN EXISTS (SELECT 1 FROM graph.node WHERE id = v_id AND retired_at IS NOT NULL);

        IF r ->> 'project' IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM graph.edge e WHERE e.src = v_id AND e.rel = 'component_of'
        ) THEN
            INSERT INTO graph.node(kind, key) VALUES ('project', r ->> 'project')
            ON CONFLICT (account, kind, key) DO NOTHING;
            SELECT id INTO v_project FROM graph.node
            WHERE account = v_account AND kind = 'project' AND key = r ->> 'project' AND retired_at IS NULL;
            IF v_project IS NOT NULL THEN
                BEGIN
                    INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind) VALUES (v_id, 'component', 'component_of', v_project, 'project');
                    v_projects := v_projects + 1;
                EXCEPTION WHEN unique_violation THEN
                    NULL;
                END;
            END IF;
        END IF;

        CONTINUE WHEN NOT v_new;
        INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind)
        SELECT gt.node_id, 'ticket', 'in_component', v_id, 'component'
        FROM public.tickets t
        JOIN graph.ticket gt ON gt.ticket_seq = t.seq AND gt.account = v_account
        JOIN graph.node tn ON tn.id = gt.node_id AND tn.retired_at IS NULL
        WHERE t.tags && v_tags
        ORDER BY t.seq;
        GET DIAGNOSTICS v_n = ROW_COUNT;
        v_tickets := v_tickets + v_n;
    END LOOP;
    RETURN jsonb_build_object('created', v_created, 'updated', v_updated, 'unchanged', v_unchanged,
                              'projects', v_projects, 'tickets', v_tickets);
END;
$$;

CREATE FUNCTION graph.import_links(p_links jsonb, p_seed text DEFAULT 'links'::text) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    r jsonb;
    v_src bigint;
    v_dst bigint;
    v_rel text;
    v_reason text;
    v_created int := 0;
    v_exists int := 0;
    v_closed int := 0;
    v_unknown jsonb := '[]'::jsonb;
    v_cycles jsonb := '[]'::jsonb;
    v_detail text;
    v_constraint text;
    v_src_retired boolean;
    v_dst_retired boolean;
    v_account text := public.instance_account();
BEGIN
    IF jsonb_typeof(p_links) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'import_links: a JSON array expected' USING ERRCODE = '22023';
    END IF;
    IF p_seed !~ '^[a-z0-9][a-z0-9-]{0,39}$' THEN
        RAISE EXCEPTION 'import_links: bad seed name %', p_seed USING ERRCODE = '22023';
    END IF;
    PERFORM set_config('hub.actor', 'seed:' || p_seed, true), set_config('hub.actor_kind', 'seed', true);
    PERFORM pg_advisory_xact_lock(193700, 109);

    -- Outer block: its rollback (on the GRIMP exception) undoes all inserts of this call at once.
    BEGIN
        FOR r IN SELECT value FROM jsonb_array_elements(p_links) LOOP
            v_rel := coalesce(r ->> 'rel', 'blocks');
            -- The node is looked up without a retirement filter: a retired ticket gets its own reason, not unknown.
            v_src := NULL; v_dst := NULL; v_src_retired := false; v_dst_retired := false;
            SELECT gt.node_id, n.retired_at IS NOT NULL INTO v_src, v_src_retired FROM public.tickets t
            JOIN graph.ticket gt ON gt.ticket_seq = t.seq AND gt.account = v_account
            JOIN graph.node n ON n.id = gt.node_id
            WHERE t.code = lower(r ->> 'src');
            SELECT gt.node_id, n.retired_at IS NOT NULL INTO v_dst, v_dst_retired FROM public.tickets t
            JOIN graph.ticket gt ON gt.ticket_seq = t.seq AND gt.account = v_account
            JOIN graph.node n ON n.id = gt.node_id
            WHERE t.code = lower(r ->> 'dst');
            v_reason := CASE
                WHEN v_src IS NULL THEN 'unknown_src'
                WHEN v_dst IS NULL THEN 'unknown_dst'
                WHEN v_src_retired THEN 'retired_src'
                WHEN v_dst_retired THEN 'retired_dst'
                WHEN v_src = v_dst THEN 'self_link'
                WHEN NOT EXISTS (SELECT 1 FROM graph.rel_shape s WHERE s.account = v_account AND s.rel = v_rel
                                 AND s.src_kind = 'ticket' AND s.dst_kind = 'ticket') THEN 'unknown_shape'
            END;
            IF v_reason IS NOT NULL THEN
                v_unknown := v_unknown || jsonb_build_object('src', r ->> 'src', 'rel', v_rel, 'dst', r ->> 'dst',
                                                             'reason', v_reason);
                CONTINUE;
            END IF;
            IF EXISTS (SELECT 1 FROM graph.edge e WHERE e.account = v_account AND e.src = v_src AND e.rel = v_rel
                       AND e.dst = v_dst AND e.valid_to IS NULL) THEN
                v_exists := v_exists + 1;
                CONTINUE;
            END IF;
            IF EXISTS (SELECT 1 FROM graph.edge e WHERE e.account = v_account AND e.src = v_src AND e.rel = v_rel
                       AND e.dst = v_dst) THEN
                v_closed := v_closed + 1;
                CONTINUE;
            END IF;
            -- Inner block: a cycle or a taken single slot is recorded and the pass goes on, so that the report gets
            -- all cycles of the file, not only the first one.
            BEGIN
                INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, evidence)
                VALUES (v_src, 'ticket', v_rel, v_dst, 'ticket',
                        jsonb_build_object('source', left(coalesce(r ->> 'source', 'body'), 100), 'seed', p_seed));
                v_created := v_created + 1;
            EXCEPTION
                WHEN SQLSTATE 'GRCYC' THEN
                    GET STACKED DIAGNOSTICS v_detail = PG_EXCEPTION_DETAIL;
                    v_cycles := v_cycles || jsonb_build_array(v_detail::jsonb);
                WHEN unique_violation THEN
                    -- edge_single: src already has another live edge of this rel (for example, another part_of parent).
                    -- Any other index (edge_live_unique) means the same edge was inserted in parallel: exists.
                    GET STACKED DIAGNOSTICS v_constraint = CONSTRAINT_NAME;
                    IF v_constraint = 'edge_single' THEN
                        v_unknown := v_unknown || jsonb_build_object('src', r ->> 'src', 'rel', v_rel,
                                                                     'dst', r ->> 'dst', 'reason', 'single_violation');
                    ELSE
                        v_exists := v_exists + 1;
                    END IF;
            END;
        END LOOP;
        IF jsonb_array_length(v_cycles) > 0 THEN
            RAISE EXCEPTION 'import_links: cycle' USING ERRCODE = 'GRIMP';
        END IF;
    EXCEPTION
        WHEN SQLSTATE 'GRIMP' THEN
            RETURN jsonb_build_object('status', 'cycle', 'created', 0, 'exists', v_exists, 'closed', v_closed,
                                      'unknown', v_unknown, 'cycles', v_cycles);
    END;
    RETURN jsonb_build_object('status', 'ok', 'created', v_created, 'exists', v_exists, 'closed', v_closed,
                              'unknown', v_unknown, 'cycles', v_cycles);
END;
$_$;

CREATE FUNCTION graph.import_people(p_people jsonb) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    r jsonb;
    v_id bigint;
    v_old graph.person;
    v_handle text;
    v_name text;
    v_email text;
    v_aliases text[];
    v_agent boolean;
    v_created int := 0;
    v_updated int := 0;
    v_unchanged int := 0;
    v_account text := public.instance_account();
BEGIN
    IF jsonb_typeof(p_people) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'import_people: a JSON array expected' USING ERRCODE = '22023';
    END IF;
    PERFORM set_config('hub.actor', 'seed:people', true), set_config('hub.actor_kind', 'seed', true);
    -- Two pods do not import at the same time on start.
    PERFORM pg_advisory_xact_lock(193700, 157);
    FOR r IN SELECT value FROM jsonb_array_elements(p_people) LOOP
        v_handle := r ->> 'handle';
        v_name := r ->> 'name';
        v_email := nullif(r ->> 'email', '');
        v_aliases := ARRAY(SELECT jsonb_array_elements_text(coalesce(r -> 'aliases', '[]'::jsonb)));
        v_agent := coalesce((r ->> 'is_agent')::boolean, false);
        INSERT INTO graph.node(kind, key) VALUES ('person', v_handle)
        ON CONFLICT (account, kind, key) DO NOTHING
        RETURNING id INTO v_id;
        IF v_id IS NULL THEN
            SELECT id INTO v_id FROM graph.node WHERE account = v_account AND kind = 'person' AND key = v_handle;
        END IF;
        SELECT * INTO v_old FROM graph.person WHERE node_id = v_id;
        IF NOT FOUND THEN
            INSERT INTO graph.person(node_id, handle, name, email, aliases, is_agent)
            VALUES (v_id, v_handle, v_name, v_email, v_aliases, v_agent);
            v_created := v_created + 1;
        ELSIF (v_old.name, v_old.email, v_old.aliases, v_old.is_agent)
              IS DISTINCT FROM (v_name, v_email, v_aliases, v_agent) THEN
            UPDATE graph.person SET name = v_name, email = v_email, aliases = v_aliases, is_agent = v_agent
            WHERE node_id = v_id;
            v_updated := v_updated + 1;
        ELSE
            v_unchanged := v_unchanged + 1;
        END IF;
    END LOOP;
    RETURN jsonb_build_object('created', v_created, 'updated', v_updated, 'unchanged', v_unchanged);
END;
$$;

CREATE FUNCTION graph.node_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    IF (NEW.id, NEW.account, NEW.kind, NEW.key, NEW.created_at)
       IS DISTINCT FROM (OLD.id, OLD.account, OLD.kind, OLD.key, OLD.created_at) THEN
        RAISE EXCEPTION 'node identity is immutable' USING ERRCODE = 'GRFRB';
    END IF;
    IF OLD.retired_at IS NOT NULL
       AND (NEW.retired_at, NEW.retire_reason) IS DISTINCT FROM (OLD.retired_at, OLD.retire_reason) THEN
        RAISE EXCEPTION 'node is retired once' USING ERRCODE = 'GRFRB';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION graph.node_label(p_node bigint) RETURNS text
    LANGUAGE sql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    SELECT coalesce(t.code, n.key)
    FROM graph.node n
    LEFT JOIN graph.ticket gt ON gt.node_id = n.id
    LEFT JOIN public.tickets t ON t.seq = gt.ticket_seq
    WHERE n.id = p_node
$$;

CREATE FUNCTION graph.person_login_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_actor text := graph.require_fresh_human();
    v_person record;
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF v_actor IS DISTINCT FROM 'user:' || NEW.user_id THEN
            RAISE EXCEPTION 'a login is bound only by its own user' USING ERRCODE = 'GRLGN';
        END IF;
        SELECT p.is_agent, n.retired_at IS NOT NULL AS retired INTO v_person
        FROM graph.person p JOIN graph.node n ON n.id = p.node_id
        WHERE p.account = NEW.account AND p.handle = NEW.handle;
        IF FOUND AND v_person.retired THEN
            RAISE EXCEPTION 'person % is retired', NEW.handle USING ERRCODE = 'GRRET';
        END IF;
        IF FOUND AND v_person.is_agent THEN
            RAISE EXCEPTION 'person % is an agent: a login binds to a human', NEW.handle USING ERRCODE = 'GRLGN';
        END IF;
        NEW.actor := v_actor;
        NEW.actor_kind := 'human';
        NEW.created_at := now();
        NEW.retired_at := NULL;
        NEW.retired_by := NULL;
        RETURN NEW;
    END IF;
    IF OLD.retired_at IS NOT NULL OR NEW.retired_at IS NULL
       OR (to_jsonb(NEW) - 'retired_at' - 'retired_by') IS DISTINCT FROM (to_jsonb(OLD) - 'retired_at' - 'retired_by') THEN
        RAISE EXCEPTION 'graph.person_login: a login is only retired, once' USING ERRCODE = 'GRFRB';
    END IF;
    NEW.retired_at := now();
    NEW.retired_by := v_actor;
    RETURN NEW;
END;
$$;

CREATE FUNCTION graph.person_login_key_owner() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
BEGIN
    UPDATE mailbox.keys SET owner = NEW.handle
    WHERE id = 'user:' || NEW.user_id AND owner <> NEW.handle AND NEW.handle ~ '^[a-z0-9]+$';
    RETURN NULL;
END;
$_$;

CREATE FUNCTION graph.project_node() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    INSERT INTO graph.node(kind, key) VALUES ('project', NEW.project)
    ON CONFLICT (account, kind, key) DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE FUNCTION graph.require_fresh_human() RETURNS text
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_actor text := nullif(current_setting('hub.actor', true), '');
    v_kind text := nullif(current_setting('hub.actor_kind', true), '');
BEGIN
    IF v_actor IS NULL OR v_kind IS NULL THEN
        RAISE EXCEPTION 'hub.actor and hub.actor_kind are required' USING ERRCODE = 'GRACT';
    END IF;
    IF v_kind <> 'human' THEN
        RAISE EXCEPTION 'requires_human' USING ERRCODE = 'GRHUM';
    END IF;
    IF coalesce(current_setting('hub.fresh', true), '') <> 'on' THEN
        RAISE EXCEPTION 'fresh_session_required: sign in again (not older than 5 minutes)' USING ERRCODE = 'GRFSH';
    END IF;
    RETURN v_actor;
END;
$$;

CREATE FUNCTION graph.ticket_node() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_id bigint;
BEGIN
    INSERT INTO graph.node(kind, key) VALUES ('ticket', NEW.seq::text)
    ON CONFLICT (account, kind, key) DO NOTHING
    RETURNING id INTO v_id;
    IF v_id IS NULL THEN
        SELECT id INTO v_id FROM graph.node WHERE account = public.instance_account() AND kind = 'ticket' AND key = NEW.seq::text;
    END IF;
    INSERT INTO graph.ticket(node_id, ticket_seq) VALUES (v_id, NEW.seq)
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE FUNCTION mailbox.exchange(p_key text, p_owner text, p_agent text, p_msg text, p_now timestamp with time zone, p_trusted boolean) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
declare
    k mailbox.keys;
    after_id bigint;
    out jsonb;
    top_id bigint;
begin
    -- An installation that expects a legacy history import (public.instance.mailbox_legacy_import) stays closed until
    -- the import of that source is written; an installation that expects none is open from the start.
    if exists (select 1 from public.instance i
                where i.id and i.mailbox_legacy_import is not null
                  and not exists (select 1 from mailbox.imports m where m.source = i.mailbox_legacy_import)) then
        return jsonb_build_object('status', 'moving', 'messages', '[]'::jsonb);
    end if;

    -- A known key: while it waits for approval, the owner prefix can still be corrected.
    update mailbox.keys
       set last_seen_at = p_now,
           owner = case when status = 'pending' then p_owner else owner end
     where id = p_key
    returning * into k;

    if not found then
        if p_trusted then
            insert into mailbox.keys (id, owner, status, created_at, approved_at, last_seen_at)
            values (p_key, p_owner, 'approved', p_now, p_now, p_now)
            on conflict (id) do update set last_seen_at = excluded.last_seen_at
            returning * into k;
        else
            -- Write brake without authentication: no more than 20 keys pending.
            insert into mailbox.keys (id, owner, status, created_at, last_seen_at)
            select p_key, p_owner, 'pending', p_now, p_now
             where (select count(*) from mailbox.keys where status = 'pending') < 20
            on conflict (id) do update set last_seen_at = excluded.last_seen_at
            returning * into k;
            return case when found
                then jsonb_build_object('status', 'pending', 'registered', true, 'messages', '[]'::jsonb)
                else jsonb_build_object('status', 'pending', 'messages', '[]'::jsonb) end;
        end if;
    end if;

    if k.owner <> p_owner then
        return jsonb_build_object('status', 'wrong-owner', 'messages', '[]'::jsonb);
    end if;
    if k.status <> 'approved' then
        return jsonb_build_object('status', k.status, 'messages', '[]'::jsonb);
    end if;

    -- A new session starts from the head of the mailbox. A repeat within 30 s after the FIRST delivery of a batch
    -- (a failed curl) does not acknowledge it but delivers it again.
    insert into mailbox.cursors (key_id, agent, last_id, pending_id, last_seen_at)
    select k.id, p_agent, h.id, h.id, p_now
      from (select coalesce(max(id), 0) as id from mailbox.messages) h
    on conflict (key_id, agent) do update set
        last_seen_at = excluded.last_seen_at,
        last_id = case when p_now - mailbox.cursors.pending_at < interval '30 seconds'
                       then mailbox.cursors.last_id
                       else greatest(mailbox.cursors.last_id, mailbox.cursors.pending_id) end
    returning mailbox.cursors.last_id into after_id;

    insert into mailbox.messages (key_id, agent, text, created_at) values (k.id, p_agent, p_msg, p_now);

    select coalesce(jsonb_agg(jsonb_build_object('id', m.id, 'from', m.agent, 'text', m.text) order by m.id), '[]'::jsonb),
           max(m.id)
      into out, top_id
      from (select id, agent, text from mailbox.messages
             where id > after_id and agent <> p_agent
             order by id limit 20) m;

    if top_id is not null then
        update mailbox.cursors
           set pending_at = case when top_id > pending_id then p_now else pending_at end,
               pending_id = greatest(pending_id, top_id)
         where key_id = k.id and agent = p_agent;
    end if;
    return jsonb_build_object('status', 'approved', 'messages', out);
end;
$$;

CREATE FUNCTION mailbox.import_history(snapshot jsonb) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
declare
    n_keys integer;
    n_cursors integer;
    n_messages integer;
    top bigint;
    differs integer;
    expected text;
begin
    perform pg_advisory_xact_lock(193700, 104);
    -- Only the source that the installation expects (public.instance.mailbox_legacy_import); none expected, none taken.
    select mailbox_legacy_import into expected from public.instance where id;
    if expected is null or snapshot->>'source' is distinct from expected then
        raise exception 'unsupported mailbox source: % (expected: %)', snapshot->>'source', coalesce(expected, 'none');
    end if;
    if jsonb_typeof(snapshot->'keys') is distinct from 'array'
       or jsonb_typeof(snapshot->'cursors') is distinct from 'array'
       or jsonb_typeof(snapshot->'messages') is distinct from 'array' then
        raise exception 'invalid mailbox snapshot';
    end if;
    if exists (select 1 from jsonb_array_elements(snapshot->'keys') k(v) where v->>'id' !~ '^sha256:[0-9a-f]{64}$') then
        raise exception 'invalid key id: keys must arrive hashed';
    end if;

    create temp table in_keys on commit drop as
    select v->>'id' as id, v->>'owner' as owner, v->>'status' as status,
           'epoch'::timestamptz + (v->>'created_at_ms')::bigint * interval '1 millisecond' as created_at,
           'epoch'::timestamptz + (v->>'approved_at_ms')::bigint * interval '1 millisecond' as approved_at,
           'epoch'::timestamptz + (v->>'last_seen_ms')::bigint * interval '1 millisecond' as last_seen_at
      from jsonb_array_elements(snapshot->'keys') k(v);
    create temp table in_cursors on commit drop as
    select v->>'key_id' as key_id, v->>'agent' as agent, (v->>'last_id')::bigint as last_id,
           (v->>'pending_id')::bigint as pending_id,
           'epoch'::timestamptz + (v->>'pending_at_ms')::bigint * interval '1 millisecond' as pending_at,
           'epoch'::timestamptz + (v->>'last_seen_ms')::bigint * interval '1 millisecond' as last_seen_at
      from jsonb_array_elements(snapshot->'cursors') c(v);
    create temp table in_messages on commit drop as
    select (v->>'id')::bigint as id, v->>'key_id' as key_id, v->>'agent' as agent, v->>'text' as text,
           'epoch'::timestamptz + (v->>'created_at_ms')::bigint * interval '1 millisecond' as created_at
      from jsonb_array_elements(snapshot->'messages') m(v);

    -- The last contact time and the cursors live on in the hub; only what does not change is compared.
    select count(*) into differs from in_keys i join mailbox.keys k using (id)
     where (k.created_at, k.owner) is distinct from (i.created_at, i.owner);
    select differs + count(*) into differs from in_messages i join mailbox.messages m using (id)
     where (m.key_id, m.agent, m.text, m.created_at) is distinct from (i.key_id, i.agent, i.text, i.created_at);
    select differs + count(*) into differs from in_messages i
     where exists (select 1 from mailbox.imports)
       and not exists (select 1 from mailbox.messages m where m.id = i.id);
    if differs > 0 then
        raise exception 'mailbox snapshot differs from the imported history (% rows)', differs;
    end if;

    insert into mailbox.keys select id, owner, status, created_at, approved_at, last_seen_at from in_keys
    on conflict (id) do nothing;
    insert into mailbox.messages (id, key_id, agent, text, created_at) overriding system value
    select id, key_id, agent, text, created_at from in_messages
    on conflict (id) do nothing;
    insert into mailbox.cursors select key_id, agent, last_id, pending_id, pending_at, last_seen_at from in_cursors
    on conflict (key_id, agent) do nothing;

    select count(*) from in_keys into n_keys;
    select count(*) from in_cursors into n_cursors;
    select count(*), coalesce(max(id), 0) from in_messages into n_messages, top;
    perform setval(pg_get_serial_sequence('mailbox.messages', 'id'),
                   greatest(top, (select coalesce(max(id), 0) from mailbox.messages), 1));
    insert into mailbox.imports values (expected, now(), n_keys, n_cursors, n_messages, top)
    on conflict (source) do nothing;
    return jsonb_build_object('keys', n_keys, 'cursors', n_cursors, 'messages', n_messages, 'max_id', top);
end;
$_$;

CREATE FUNCTION mailbox.key_status_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    IF coalesce(current_setting('mailbox.reviewing', true), '') IS DISTINCT FROM NEW.id THEN
        RAISE EXCEPTION 'mailbox key status changes only through mailbox.review_key' USING ERRCODE = 'GRFRB';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE mailbox.keys (
    id text NOT NULL,
    owner text NOT NULL,
    status text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    approved_at timestamp with time zone,
    last_seen_at timestamp with time zone NOT NULL,
    CONSTRAINT keys_id_check CHECK ((id ~ '^(sha256:[0-9a-f]{64}|user:.+)$'::text)),
    CONSTRAINT keys_owner_check CHECK ((owner ~ '^[a-z0-9]+$'::text)),
    CONSTRAINT keys_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'approved'::text, 'revoked'::text])))
);

CREATE FUNCTION mailbox.review_key(p_id text, p_status text) RETURNS mailbox.keys
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_actor text := graph.require_fresh_human();
    v_handle text;
    k mailbox.keys;
    v_account text := public.instance_account();
BEGIN
    IF p_status IS NULL OR p_status NOT IN ('approved', 'revoked') THEN
        RAISE EXCEPTION 'status: approved or revoked' USING ERRCODE = '22023';
    END IF;
    SELECT * INTO k FROM mailbox.keys WHERE id = p_id FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no such key' USING ERRCODE = 'P0002';
    END IF;
    SELECT l.handle INTO v_handle FROM graph.person_login l
    WHERE l.account = v_account AND 'user:' || l.user_id = v_actor AND l.retired_at IS NULL;
    IF v_handle IS NULL THEN
        RAISE EXCEPTION 'no_login: bind your login on /people first' USING ERRCODE = 'GRNLG';
    END IF;
    IF v_handle <> k.owner
       AND NOT (p_status = 'revoked' AND k.status = 'pending'
                AND NOT EXISTS (SELECT 1 FROM graph.person_login l
                                WHERE l.account = v_account AND l.handle = k.owner AND l.retired_at IS NULL)) THEN
        RAISE EXCEPTION 'not_key_owner: key of %, you are %', k.owner, v_handle USING ERRCODE = 'GROWN';
    END IF;
    PERFORM set_config('mailbox.reviewing', p_id, true);
    UPDATE mailbox.keys SET status = p_status,
           approved_at = CASE WHEN p_status = 'approved' THEN coalesce(approved_at, now()) ELSE approved_at END
    WHERE id = p_id
    RETURNING * INTO k;
    PERFORM set_config('mailbox.reviewing', '', true);
    INSERT INTO mailbox.key_review (key_id, status, owner, actor, actor_kind)
    VALUES (p_id, p_status, k.owner, v_actor, 'human');
    RETURN k;
END;
$$;

CREATE FUNCTION plan.all_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    PERFORM plan.mark_all(public.instance_account());
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.baseline_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_who text[] := plan.require_actor(ARRAY['human']);
    v_scope plan.scope;
    v_live bigint;
BEGIN
    SELECT * INTO v_scope FROM plan.scope s
    WHERE s.account = NEW.account AND s.root_seq = NEW.root_seq AND s.valid_to IS NULL;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no live scope for root %', NEW.root_seq USING ERRCODE = 'PLSCP';
    END IF;
    SELECT b.id INTO v_live FROM plan.baseline_live b WHERE b.account = NEW.account AND b.root_seq = NEW.root_seq;
    IF NEW.supersedes IS DISTINCT FROM v_live THEN
        RAISE EXCEPTION 'baseline % is not the live one (live: %)', NEW.supersedes, v_live USING ERRCODE = 'PLSUP';
    END IF;
    NEW.scope_id := v_scope.id;
    NEW.max_defaulted_pct := v_scope.max_defaulted_pct;
    IF NOT NEW.accepted_defaults AND NEW.defaulted_pct > NEW.max_defaulted_pct THEN
        RAISE EXCEPTION 'defaulted estimates on the chain: % %% over % %%', NEW.defaulted_pct, NEW.max_defaulted_pct
            USING ERRCODE = 'PLDEF';
    END IF;
    NEW.approved_at := now();
    NEW.approved_by := v_who[1];
    NEW.approved_kind := v_who[2];
    RETURN NEW;
END;
$$;

CREATE FUNCTION plan.current_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_tasks bigint[];
BEGIN
    IF TG_OP = 'INSERT' THEN
        v_tasks := ARRAY(SELECT (x ->> 'seq')::bigint FROM jsonb_array_elements(NEW.result -> 'tasks') AS x);
    ELSE
        v_tasks := ARRAY(
            SELECT coalesce(n.seq, o.seq)
            FROM (SELECT (x ->> 'seq')::bigint AS seq, x -> 'end' AS e
                  FROM jsonb_array_elements(coalesce(NEW.result -> 'tasks', '[]'::jsonb)) AS x) AS n
            FULL JOIN (SELECT (x ->> 'seq')::bigint AS seq, x -> 'end' AS e
                       FROM jsonb_array_elements(coalesce(OLD.result -> 'tasks', '[]'::jsonb)) AS x) AS o
              ON o.seq = n.seq
            WHERE n.seq IS NULL OR o.seq IS NULL OR n.e IS DISTINCT FROM o.e);
    END IF;
    IF cardinality(v_tasks) = 0 THEN
        RETURN NULL;
    END IF;
    PERFORM plan.mark(NEW.account, ARRAY(
        SELECT DISTINCT r
        FROM graph.ticket s
        JOIN graph.edge e ON e.src = s.node_id AND e.rel = 'blocks' AND e.state = 'confirmed' AND e.valid_to IS NULL
        JOIN graph.ticket d ON d.node_id = e.dst
        CROSS JOIN LATERAL unnest(plan.roots_of(NEW.account, ARRAY[d.ticket_seq])) AS r
        WHERE s.account = NEW.account AND s.ticket_seq = ANY (v_tasks) AND r <> NEW.root_seq
          AND NOT r = ANY (plan.roots_of(NEW.account, ARRAY[s.ticket_seq]))
        ORDER BY r));
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.dirty_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.marked := 1;
        NEW.done := 0;
        NEW.since := now();
        RETURN NEW;
    END IF;
    IF (NEW.account, NEW.root_seq) IS DISTINCT FROM (OLD.account, OLD.root_seq)
       OR NEW.marked < OLD.marked OR NEW.done < OLD.done THEN
        RAISE EXCEPTION 'plan.dirty: counters only grow' USING ERRCODE = 'GRFRB';
    END IF;
    IF NEW.done <> OLD.done THEN
        PERFORM plan.require_actor(ARRAY['service:plan']);
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION plan.edge_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_seqs bigint[];
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.state IS NOT DISTINCT FROM OLD.state AND NEW.valid_to IS NOT DISTINCT FROM OLD.valid_to THEN
        RETURN NULL;
    END IF;
    v_seqs := ARRAY(SELECT gt.ticket_seq FROM graph.ticket gt WHERE gt.account = NEW.account AND gt.node_id IN (NEW.src, NEW.dst));
    IF NEW.rel = 'assigned_to' THEN
        v_seqs := v_seqs || ARRAY(
            SELECT gt.ticket_seq FROM graph.edge e JOIN graph.ticket gt ON gt.node_id = e.src
            WHERE e.account = NEW.account AND e.dst = NEW.dst AND e.rel = 'assigned_to' AND e.state = 'confirmed'
              AND e.valid_to IS NULL);
    END IF;
    PERFORM plan.mark(NEW.account, plan.roots_of(NEW.account, v_seqs));
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.estimate_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    PERFORM plan.mark(NEW.account, plan.roots_of(NEW.account, ARRAY[NEW.ticket_seq]));
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.flush(p_account text DEFAULT NULL::text, p_root bigint DEFAULT NULL::bigint, p_done bigint DEFAULT NULL::bigint) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_pending text[] := string_to_array(nullif(current_setting('plan.pending', true), ''), ',');
    v_own text := p_account || '/' || p_root;
BEGIN
    IF v_pending IS NULL AND v_own IS NULL THEN
        RETURN;
    END IF;
    PERFORM set_config('plan.pending', '', true);
    INSERT INTO plan.dirty AS d (account, root_seq)
    SELECT DISTINCT split_part(x, '/', 1), split_part(x, '/', 2)::bigint
    FROM unnest(coalesce(v_pending, '{}') || ARRAY[v_own]) AS x
    WHERE x IS NOT NULL
    ORDER BY 1, 2
    ON CONFLICT (account, root_seq) DO UPDATE
        SET marked = d.marked + CASE WHEN d.account || '/' || d.root_seq = ANY (v_pending) THEN 1 ELSE 0 END,
            done = CASE WHEN d.account || '/' || d.root_seq = v_own THEN greatest(d.done, p_done) ELSE d.done END,
            since = CASE WHEN d.account || '/' || d.root_seq = ANY (v_pending)
                          AND d.marked <= CASE WHEN d.account || '/' || d.root_seq = v_own
                                               THEN greatest(d.done, p_done) ELSE d.done END
                         THEN now() ELSE d.since END;
END;
$$;

CREATE FUNCTION plan.flush_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    PERFORM plan.flush();
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.import_scopes(p_scopes jsonb) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    r jsonb;
    v_root text;
    v_seq bigint;
    v_live plan.scope;
    v_ever boolean;
    v_due date;
    v_mode text;
    v_method text;
    v_green int;
    v_red int;
    v_slope int;
    v_maxdef int;
    v_tickets int := 0;
    v_created int := 0;
    v_updated int := 0;
    v_unchanged int := 0;
    v_closed int := 0;
    v_unknown jsonb := '[]'::jsonb;
    v_links jsonb := '{}'::jsonb;
    v_account text := public.instance_account();
BEGIN
    IF jsonb_typeof(p_scopes) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'import_scopes: a JSON array expected' USING ERRCODE = '22023';
    END IF;
    PERFORM set_config('hub.actor', 'seed:scopes', true), set_config('hub.actor_kind', 'seed', true);
    PERFORM pg_advisory_xact_lock(193700, 162);
    FOR r IN SELECT value FROM jsonb_array_elements(p_scopes) LOOP
        v_root := lower(r ->> 'root');
        v_seq := NULL;
        SELECT t.seq INTO v_seq FROM public.tickets t WHERE t.code = v_root;
        IF v_seq IS NULL THEN
            IF nullif(r ->> 'project', '') IS NULL OR nullif(r ->> 'summary', '') IS NULL THEN
                v_unknown := v_unknown || jsonb_build_object('root', v_root, 'reason', 'unknown_root');
                CONTINUE;
            END IF;
            INSERT INTO public.tickets(project, code, summary, body, priority, tags)
            VALUES (r ->> 'project', v_root, r ->> 'summary', coalesce(r ->> 'body', ''), nullif(r ->> 'priority', ''),
                    ARRAY(SELECT jsonb_array_elements_text(coalesce(r -> 'tags', '[]'::jsonb))))
            RETURNING seq INTO v_seq;
            v_tickets := v_tickets + 1;
        END IF;

        v_links := v_links || jsonb_build_object(v_root, graph.import_links(coalesce((
            SELECT jsonb_agg(jsonb_build_object('src', c, 'rel', 'part_of', 'dst', v_root,
                                                'source', 'scopes.toml:' || v_root) ORDER BY o)
            FROM jsonb_array_elements_text(coalesce(r -> 'children', '[]'::jsonb)) WITH ORDINALITY AS x(c, o)),
            '[]'::jsonb), 'scopes'));
        -- import_links sets the same actor; in case of another seed in the same transaction it is set again.
        PERFORM set_config('hub.actor', 'seed:scopes', true), set_config('hub.actor_kind', 'seed', true);

        v_due := (r ->> 'due_date')::date;
        v_mode := coalesce(r ->> 'mode', 'forward');
        v_method := coalesce(r ->> 'buffer_method', 'cnp');
        v_green := coalesce((r ->> 'green_a_pct')::int, 10);
        v_red := coalesce((r ->> 'red_a_pct')::int, 30);
        v_slope := coalesce((r ->> 'zone_slope_pct')::int, 60);
        v_maxdef := coalesce((r ->> 'max_defaulted_pct')::int, 20);
        v_live := NULL;
        SELECT * INTO v_live FROM plan.scope s
        WHERE s.account = v_account AND s.root_seq = v_seq AND s.valid_to IS NULL;
        v_ever := EXISTS (SELECT 1 FROM plan.scope s WHERE s.account = v_account AND s.root_seq = v_seq);
        IF NOT v_ever
           OR (v_live.id IS NOT NULL AND v_live.actor = 'seed:scopes'
               AND (v_live.due_date, v_live.mode, v_live.buffer_method, v_live.green_a_pct, v_live.red_a_pct,
                    v_live.zone_slope_pct, v_live.max_defaulted_pct)
                   IS DISTINCT FROM (v_due, v_mode, v_method, v_green, v_red, v_slope, v_maxdef)) THEN
            INSERT INTO plan.scope(root_seq, due_date, mode, buffer_method, green_a_pct, red_a_pct, zone_slope_pct,
                                   max_defaulted_pct)
            VALUES (v_seq, v_due, v_mode, v_method, v_green, v_red, v_slope, v_maxdef);
            IF v_ever THEN
                v_updated := v_updated + 1;
            ELSE
                v_created := v_created + 1;
            END IF;
        ELSIF v_live.id IS NULL THEN
            v_closed := v_closed + 1;
        ELSE
            v_unchanged := v_unchanged + 1;
        END IF;
    END LOOP;
    RETURN jsonb_build_object('tickets_created', v_tickets, 'created', v_created, 'updated', v_updated,
                              'unchanged', v_unchanged, 'closed', v_closed, 'unknown', v_unknown, 'links', v_links);
END;
$$;

CREATE FUNCTION plan.mark(p_account text, p_roots bigint[]) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_new text[] := ARRAY(SELECT p_account || '/' || r FROM unnest(p_roots) AS r WHERE r IS NOT NULL);
BEGIN
    IF cardinality(v_new) = 0 THEN
        RETURN;
    END IF;
    PERFORM set_config('plan.pending', array_to_string(ARRAY(
        SELECT DISTINCT x FROM unnest(string_to_array(nullif(current_setting('plan.pending', true), ''), ',') || v_new)
            AS x ORDER BY x), ','), true);
END;
$$;

CREATE FUNCTION plan.mark_all(p_account text) RETURNS void
    LANGUAGE sql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    SELECT plan.mark(p_account, ARRAY(SELECT root_seq FROM plan.scope WHERE account = p_account AND valid_to IS NULL))
$$;

CREATE FUNCTION plan.mark_all_now(p_account text) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    PERFORM plan.mark_all(p_account);
    PERFORM plan.flush();
END;
$$;

CREATE FUNCTION plan.mark_now(p_account text, p_roots bigint[]) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    PERFORM plan.mark(p_account, p_roots);
    PERFORM plan.flush();
END;
$$;

CREATE FUNCTION plan.require_actor(p_kinds text[]) RETURNS text[]
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_actor text := nullif(current_setting('hub.actor', true), '');
    v_kind text := nullif(current_setting('hub.actor_kind', true), '');
BEGIN
    IF v_actor IS NULL OR v_kind IS NULL THEN
        RAISE EXCEPTION 'hub.actor and hub.actor_kind are required' USING ERRCODE = 'GRACT';
    END IF;
    IF NOT v_kind = ANY (p_kinds) THEN
        IF p_kinds = ARRAY['human'] THEN
            RAISE EXCEPTION 'requires_human' USING ERRCODE = 'GRHUM';
        END IF;
        RAISE EXCEPTION 'actor kind % is not allowed here (allowed: %)', v_kind, array_to_string(p_kinds, ', ')
            USING ERRCODE = 'PLKND';
    END IF;
    RETURN ARRAY[v_actor, v_kind];
END;
$$;

CREATE FUNCTION plan.root_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    PERFORM plan.mark(NEW.account, ARRAY[NEW.root_seq]);
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.roots_of(p_account text, p_seqs bigint[]) RETURNS bigint[]
    LANGUAGE sql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    WITH RECURSIVE up(seq) AS (
        SELECT s FROM unnest(p_seqs) AS s WHERE s IS NOT NULL
        UNION
        SELECT pt.ticket_seq FROM up
        JOIN graph.ticket ct ON ct.account = p_account AND ct.ticket_seq = up.seq
        JOIN graph.edge e ON e.account = p_account AND e.src = ct.node_id AND e.rel = 'part_of'
         AND e.state = 'confirmed' AND e.valid_to IS NULL
        JOIN graph.ticket pt ON pt.node_id = e.dst
    )
    SELECT coalesce(array_agg(DISTINCT s.root_seq ORDER BY s.root_seq), '{}')
    FROM up JOIN plan.scope s ON s.account = p_account AND s.root_seq = up.seq AND s.valid_to IS NULL
$$;

CREATE FUNCTION plan.scope_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_kind text := nullif(current_setting('hub.actor_kind', true), '');
    v_old plan.scope;
BEGIN
    IF v_kind IS NULL OR v_kind IN ('human', 'seed') THEN
        RETURN NEW;   -- an empty actor is refused by version_guard
    END IF;
    SELECT * INTO v_old FROM plan.scope s
    WHERE s.account = NEW.account AND s.root_seq = NEW.root_seq AND s.valid_to IS NULL;
    IF (NEW.green_a_pct, NEW.red_a_pct, NEW.zone_slope_pct, NEW.max_defaulted_pct)
       IS DISTINCT FROM (coalesce(v_old.green_a_pct, 10), coalesce(v_old.red_a_pct, 30),
                         coalesce(v_old.zone_slope_pct, 60), coalesce(v_old.max_defaulted_pct, 20)) THEN
        RAISE EXCEPTION 'scope thresholds are changed by a human only' USING ERRCODE = 'PLTHR';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION plan.service_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_who text[] := plan.require_actor(ARRAY['service:plan']);
BEGIN
    NEW.actor := v_who[1];
    RETURN NEW;
END;
$$;

CREATE FUNCTION plan.ticket_dirty() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_account text := public.instance_account();
BEGIN
    PERFORM plan.mark(v_account, plan.roots_of(v_account, ARRAY[NEW.seq] || ARRAY(
        SELECT d.ticket_seq FROM graph.ticket s
        JOIN graph.edge e ON e.src = s.node_id AND e.rel = 'blocks' AND e.state = 'confirmed' AND e.valid_to IS NULL
        JOIN graph.ticket d ON d.node_id = e.dst
        WHERE s.account = v_account AND s.ticket_seq = NEW.seq)));
    RETURN NULL;
END;
$$;

CREATE FUNCTION plan.version_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    v_who text[] := plan.require_actor(string_to_array(TG_ARGV[1], ','));
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.valid_from := now();
        NEW.valid_to := NULL;
        NEW.closed_by := NULL;
        NEW.actor := v_who[1];
        NEW.actor_kind := v_who[2];
        PERFORM set_config('plan.closing', '1', true);
        EXECUTE format('UPDATE %I.%I SET valid_to = now() WHERE account = $1 AND %I::text = $2 AND valid_to IS NULL',
                       TG_TABLE_SCHEMA, TG_TABLE_NAME, TG_ARGV[0])
            USING NEW.account, to_jsonb(NEW) ->> TG_ARGV[0];
        PERFORM set_config('plan.closing', '', true);
        RETURN NEW;
    END IF;
    IF OLD.valid_to IS NOT NULL OR NEW.valid_to IS NULL
       OR (to_jsonb(NEW) - 'valid_to' - 'closed_by') IS DISTINCT FROM (to_jsonb(OLD) - 'valid_to' - 'closed_by') THEN
        RAISE EXCEPTION '%.%: a version is only closed, once', TG_TABLE_SCHEMA, TG_TABLE_NAME USING ERRCODE = 'GRFRB';
    END IF;
    IF coalesce(current_setting('plan.closing', true), '') <> '1' AND v_who[2] <> 'human' THEN
        RAISE EXCEPTION '%.%: only a human closes a version without a new one', TG_TABLE_SCHEMA, TG_TABLE_NAME
            USING ERRCODE = 'GRFRB';
    END IF;
    NEW.valid_to := now();
    NEW.closed_by := v_who[1];
    RETURN NEW;
END;
$_$;

CREATE FUNCTION public.doc_apply(p_repo text, p_path text, p_extract jsonb, p_close boolean DEFAULT true, p_mark boolean DEFAULT true) RETURNS integer[]
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_opened int;
    v_closed int;
BEGIN
    UPDATE public.ticket_documents AS l SET valid_to = clock_timestamp()
    WHERE p_close AND l.repo = p_repo AND l.path = p_path AND l.valid_to IS NULL
      AND NOT EXISTS (SELECT 1 FROM jsonb_to_recordset(p_extract->'tickets') AS x(seq bigint, rel text)
                      WHERE x.seq = l.ticket_seq AND x.rel = l.rel);
    GET DIAGNOSTICS v_closed = ROW_COUNT;
    INSERT INTO public.ticket_documents(ticket_seq, repo, path, rel, valid_from)
    SELECT x.seq, p_repo, p_path, x.rel, clock_timestamp()
    FROM jsonb_to_recordset(p_extract->'tickets') AS x(seq bigint, rel text)
    WHERE NOT EXISTS (SELECT 1 FROM public.ticket_documents l
                      WHERE l.ticket_seq = x.seq AND l.repo = p_repo AND l.path = p_path AND l.rel = x.rel
                        AND l.valid_to IS NULL);
    GET DIAGNOSTICS v_opened = ROW_COUNT;

    UPDATE public.document_ref AS r SET valid_to = clock_timestamp()
    WHERE r.repo = p_repo AND r.path = p_path AND r.valid_to IS NULL
      AND NOT EXISTS (SELECT 1 FROM jsonb_to_recordset(p_extract->'refs') AS x(repo text, path text)
                      WHERE x.repo = r.target_repo AND x.path = r.target_path);
    INSERT INTO public.document_ref(repo, path, target_repo, target_path, valid_from)
    SELECT p_repo, p_path, x.repo, x.path, clock_timestamp()
    FROM jsonb_to_recordset(p_extract->'refs') AS x(repo text, path text)
    WHERE NOT EXISTS (SELECT 1 FROM public.document_ref r
                      WHERE r.repo = p_repo AND r.path = p_path AND r.target_repo = x.repo
                        AND r.target_path = x.path AND r.valid_to IS NULL);

    UPDATE public.documents
    SET derived_sha256 = CASE WHEN jsonb_array_length(p_extract->'problems') = 0 THEN sha256 END
    WHERE p_mark AND repo = p_repo AND path = p_path
      AND derived_sha256 IS DISTINCT FROM CASE WHEN jsonb_array_length(p_extract->'problems') = 0 THEN sha256 END;
    RETURN ARRAY[v_opened, v_closed];
END;
$$;

CREATE FUNCTION public.doc_date(p_path text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    SELECT substring(public.doc_name(p_path) from '^([0-9]{4}-[0-9]{2}-[0-9]{2})-')
$$;

CREATE FUNCTION public.doc_extract(p_repo text, p_path text, p_body text, p_known text[], p_labels jsonb) RETURNS jsonb
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    v_text text := public.doc_strip_code(p_body);
    v_repos text[] := ARRAY(SELECT repo FROM public.document_repository ORDER BY repo);
    v_tickets jsonb;
    v_refs jsonb;
    v_problems jsonb;
    v_dangling jsonb;
BEGIN
    WITH tok AS (
        SELECT t.rel, t.token, lower(btrim(t.token)) AS low
        FROM public.doc_ticket_tokens(v_text, p_labels) AS t
    ), res AS (
        SELECT tok.rel, tok.token,
               CASE WHEN tok.low ~ '^[0-9abcdefghjkmnpqrstvwxyz]{1,4}$'
                        THEN (SELECT seq FROM public.tickets WHERE code = tok.low)
                    ELSE (SELECT seq FROM public.tickets WHERE legacy_id = btrim(tok.token)) END AS seq
        FROM tok
        WHERE tok.low ~ '^[0-9abcdefghjkmnpqrstvwxyz]{1,4}$' OR btrim(tok.token) ~ '^[0-9a-f]{16}$'
    ), uniq AS (
        SELECT DISTINCT rel, seq FROM res WHERE seq IS NOT NULL
    )
    SELECT coalesce((SELECT jsonb_agg(jsonb_build_object('seq', seq, 'rel', rel) ORDER BY rel, seq) FROM uniq), '[]'),
           coalesce((SELECT jsonb_agg(p ORDER BY p->>'kind', p->>'detail') FROM (
               SELECT DISTINCT jsonb_build_object('kind', 'unknown_code', 'detail', left(token, 100)) AS p
               FROM res WHERE seq IS NULL
               UNION ALL
               SELECT jsonb_build_object('kind', 'too_many_codes', 'detail', rel || ': ' || count(*))
               FROM uniq GROUP BY rel HAVING count(*) > 20
           ) x), '[]')
    INTO v_tickets, v_problems;

    -- A path without a repo that is not in the own repo but is live in exactly one other registry repo leads there:
    -- specs often link to documents of a neighbouring repo by a bare path.
    WITH raw AS (
        SELECT r.target_repo, r.target_path,
               r.target_repo = p_repo AND NOT (r.target_path = ANY (coalesce(p_known, ARRAY(
                   SELECT path FROM public.documents WHERE repo = p_repo AND retired_at IS NULL)))) AS missing
        FROM public.doc_path_refs(v_text, p_repo, p_path, v_repos) AS r
    ), fixed AS (
        SELECT CASE WHEN raw.missing AND o.n = 1 THEN o.repo ELSE raw.target_repo END AS target_repo,
               raw.target_path, raw.missing AND coalesce(o.n, 0) <> 1 AS dangling
        FROM raw
        LEFT JOIN LATERAL (
            SELECT count(*) AS n, min(d.repo) AS repo FROM public.documents d
            WHERE raw.missing AND d.path = raw.target_path AND d.repo <> p_repo AND d.retired_at IS NULL
        ) o ON true
    )
    SELECT coalesce(jsonb_agg(DISTINCT jsonb_build_object('repo', target_repo, 'path', target_path)), '[]'),
           coalesce(jsonb_agg(jsonb_build_object('kind', 'dangling_link', 'detail', left(target_path, 100))
                              ORDER BY target_path) FILTER (WHERE dangling AND p_known IS NOT NULL), '[]')
    INTO v_refs, v_dangling
    FROM fixed;
    RETURN jsonb_build_object('tickets', v_tickets, 'refs', v_refs, 'problems', v_problems || v_dangling);
END;
$_$;

CREATE FUNCTION public.doc_link_label_pattern(p_labels jsonb, p_key text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
    SELECT '(?:' || string_agg(regexp_replace(l, '([][\\.^$*+?(){}|])', '\\\1', 'g'), '|' ORDER BY n) || ')'
    FROM jsonb_array_elements_text(p_labels -> p_key) WITH ORDINALITY AS x(l, n)
$_$;

CREATE FUNCTION public.doc_link_labels_valid(p_labels jsonb) RETURNS boolean
    LANGUAGE plpgsql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    v_key text;
BEGIN
    IF jsonb_typeof(p_labels) IS DISTINCT FROM 'object' THEN
        RETURN false;
    END IF;
    IF ARRAY(SELECT k FROM jsonb_object_keys(p_labels) AS k ORDER BY k) <> ARRAY['and', 'closes', 'describes'] THEN
        RETURN false;
    END IF;
    FOREACH v_key IN ARRAY ARRAY['and', 'closes', 'describes'] LOOP
        IF jsonb_typeof(p_labels -> v_key) <> 'array' OR jsonb_array_length(p_labels -> v_key) NOT BETWEEN 1 AND 20 THEN
            RETURN false;
        END IF;
        IF EXISTS (
            SELECT 1 FROM jsonb_array_elements(p_labels -> v_key) AS e(value)
            WHERE jsonb_typeof(value) <> 'string'
               OR value #>> '{}' !~ '^[^`[:cntrl:][:space:]]([^`[:cntrl:]]{0,38}[^`[:cntrl:][:space:]])?$'
        ) THEN
            RETURN false;
        END IF;
    END LOOP;
    RETURN true;
END;
$_$;

CREATE FUNCTION public.doc_link_rel(p_src text, p_dst text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    SELECT CASE WHEN public.doc_role(p_src) = 'plan' AND public.doc_role(p_dst) IN ('spec', 'intent') THEN 'implements'
                WHEN public.doc_role(p_src) = 'spec' AND public.doc_role(p_dst) = 'intent' THEN 'specifies'
                ELSE 'references' END
$$;

CREATE FUNCTION public.doc_name(p_path text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
    SELECT CASE WHEN array_length(s, 1) >= 5 AND s[array_length(s, 1)] IN ('intent.md', 'spec.md', 'plan.md', 'design.md')
                THEN s[array_length(s, 1) - 1]
                ELSE regexp_replace(s[array_length(s, 1)], '\.md$', '') END
    FROM (SELECT string_to_array(p_path, '/') AS s) x
$_$;

CREATE FUNCTION public.doc_path_refs(p_text text, p_repo text, p_path text, p_repos text[]) RETURNS TABLE(target_repo text, target_path text)
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
    WITH found AS (
        SELECT x AS r, m[3] AS p
        FROM regexp_matches(p_text,
            'https?://github\.com/([A-Za-z0-9-]+)/([A-Za-z0-9._-]+)/(?:blob|tree|raw)/[^/[:space:]]+/(docs/superpowers/(?:intents|specs|plans)/[^][[:space:]`''"()<>{}|*,;!?#]*\.md)',
            'g') AS m
        JOIN unnest(p_repos) AS x ON lower(x) = lower(m[1] || '/' || m[2])
        UNION ALL
        SELECT coalesce((SELECT x FROM unnest(p_repos) AS x
                         WHERE split_part(x, '/', 2) = (regexp_match(m[2], '([^/]*)/$'))[1]
                           AND m[2] <> '' LIMIT 1), p_repo),
               m[3]
        FROM regexp_matches(regexp_replace(p_text, 'https?://[^[:space:]]*', ' ', 'g'),
            '(^|[^A-Za-z0-9_./-])((?:[A-Za-z0-9._-]*/)*)(docs/superpowers/(?:intents|specs|plans)/[^][[:space:]`''"()<>{}|*,;!?#]*\.md)',
            'g') AS m
    )
    SELECT DISTINCT r, p FROM found
    WHERE public.doc_valid_path(p) AND (r, p) <> (p_repo, p_path)
$_$;

CREATE FUNCTION public.doc_reconcile(p_repo text, p_path text, p_revision text) RETURNS integer[]
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_doc public.documents;
    v_node bigint;
    v_role text := public.doc_role(p_path);
    v_topic text := public.doc_topic(p_path);
    v_date text := public.doc_date(p_path);
    v_want jsonb := '[]'::jsonb;
    v_cand bigint[];
    v_opened int := 0;
    v_closed int := 0;
    v_proposed int := 0;
    v_n int;
    v_account text := public.instance_account();
BEGIN
    SELECT * INTO v_doc FROM public.documents WHERE repo = p_repo AND path = p_path;
    SELECT node_id INTO v_node FROM graph.document WHERE account = v_account AND repo = p_repo AND path = p_path;
    IF v_doc IS NULL OR v_node IS NULL THEN
        RETURN ARRAY[0, 0, 0];
    END IF;
    IF v_doc.retired_at IS NULL THEN
        -- Explicit links to live documents.
        SELECT coalesce(jsonb_agg(DISTINCT jsonb_build_object(
                   'rel', public.doc_link_rel(p_path, r.target_path), 'dst', gd.node_id, 'dst_kind', 'document',
                   'state', 'confirmed', 'rule', 'link')), '[]')
        INTO v_want
        FROM public.document_ref r
        JOIN public.documents t ON t.repo = r.target_repo AND t.path = r.target_path AND t.retired_at IS NULL
        JOIN graph.document gd ON gd.account = v_account AND gd.repo = t.repo AND gd.path = t.path
        WHERE r.repo = p_repo AND r.path = p_path AND r.valid_to IS NULL;

        -- The repository project from the registry.
        v_want := v_want || coalesce((
            SELECT jsonb_agg(jsonb_build_object('rel', 'in_project', 'dst', n.id, 'dst_kind', 'project',
                                                'state', 'confirmed', 'rule', 'project'))
            FROM public.document_repository dr
            JOIN graph.node n ON n.account = v_account AND n.kind = 'project' AND n.key = dr.project
            WHERE dr.repo = p_repo), '[]');

        -- Topic, only within the repository: exactly one candidate of the same topic with a date not later than its own.
        IF v_role = 'plan' AND NOT EXISTS (
            SELECT 1 FROM jsonb_array_elements(v_want) w WHERE w->>'rule' = 'link' AND w->>'rel' = 'implements'
              AND public.doc_role((SELECT path FROM graph.document WHERE node_id = (w->>'dst')::bigint)) = 'spec'
        ) THEN
            v_cand := ARRAY(
                SELECT gd.node_id FROM public.documents d
                JOIN graph.document gd ON gd.account = v_account AND gd.repo = d.repo AND gd.path = d.path
                WHERE d.repo = p_repo AND public.doc_topic(d.path) = v_topic AND d.retired_at IS NULL
                  AND public.doc_role(d.path) = 'spec' AND d.path <> p_path
                  AND (v_date IS NULL OR public.doc_date(d.path) IS NULL OR public.doc_date(d.path) <= v_date));
            IF cardinality(v_cand) = 1 THEN
                v_want := v_want || jsonb_build_array(jsonb_build_object('rel', 'implements', 'dst', v_cand[1],
                    'dst_kind', 'document', 'state', 'proposed', 'rule', 'topic'));
            END IF;
        END IF;
        IF (v_role = 'spec' OR (v_role = 'plan' AND NOT EXISTS (
                SELECT 1 FROM public.documents d
                WHERE d.repo = p_repo AND public.doc_topic(d.path) = v_topic AND d.retired_at IS NULL
                  AND public.doc_role(d.path) = 'spec' AND d.path <> p_path)))
           AND NOT EXISTS (
            SELECT 1 FROM jsonb_array_elements(v_want) w WHERE w->>'rule' = 'link'
              AND public.doc_role((SELECT path FROM graph.document WHERE node_id = (w->>'dst')::bigint)) = 'intent'
        ) THEN
            v_cand := ARRAY(
                SELECT gd.node_id FROM public.documents d
                JOIN graph.document gd ON gd.account = v_account AND gd.repo = d.repo AND gd.path = d.path
                WHERE d.repo = p_repo AND public.doc_topic(d.path) = v_topic AND d.retired_at IS NULL
                  AND public.doc_role(d.path) = 'intent' AND d.path <> p_path
                  AND (v_date IS NULL OR public.doc_date(d.path) IS NULL OR public.doc_date(d.path) <= v_date));
            IF cardinality(v_cand) = 1 THEN
                v_want := v_want || jsonb_build_array(jsonb_build_object(
                    'rel', CASE WHEN v_role = 'spec' THEN 'specifies' ELSE 'implements' END, 'dst', v_cand[1],
                    'dst_kind', 'document', 'state', 'proposed', 'rule', 'topic'));
            END IF;
        END IF;
    END IF;

    -- Extra edges from git.
    UPDATE graph.edge e SET valid_to = now(), close_reason = 'removed', closed_by = current_setting('hub.actor')
    WHERE e.account = v_account AND e.src = v_node AND e.valid_to IS NULL AND e.evidence->>'source' = 'git'
      AND NOT (e.state = 'confirmed' AND e.confirmed_kind = 'human')
      AND NOT EXISTS (SELECT 1 FROM jsonb_to_recordset(v_want) AS w(rel text, dst bigint)
                      WHERE w.rel = e.rel AND w.dst = e.dst);
    GET DIAGNOSTICS v_n = ROW_COUNT;
    v_closed := v_closed + v_n;
    -- A proposal that an explicit link now gives.
    UPDATE graph.edge e SET valid_to = now(), close_reason = 'superseded', closed_by = current_setting('hub.actor')
    WHERE e.account = v_account AND e.src = v_node AND e.valid_to IS NULL AND e.state = 'proposed'
      AND e.evidence->>'source' = 'git'
      AND EXISTS (SELECT 1 FROM jsonb_to_recordset(v_want) AS w(rel text, dst bigint, state text)
                  WHERE w.rel = e.rel AND w.dst = e.dst AND w.state = 'confirmed');
    GET DIAGNOSTICS v_n = ROW_COUNT;
    v_closed := v_closed + v_n;

    WITH ins AS (
        INSERT INTO graph.edge(src, src_kind, rel, dst, dst_kind, state, evidence)
        SELECT v_node, 'document', w.rel, w.dst, w.dst_kind, w.state,
               jsonb_build_object('source', 'git', 'rule', w.rule, 'revision', coalesce(p_revision, v_doc.source_revision))
        FROM jsonb_to_recordset(v_want) AS w(rel text, dst bigint, dst_kind text, state text, rule text)
        WHERE NOT EXISTS (SELECT 1 FROM graph.edge e
                          WHERE e.account = v_account AND e.src = v_node AND e.rel = w.rel AND e.dst = w.dst
                            AND e.valid_to IS NULL)
          AND NOT EXISTS (SELECT 1 FROM graph.node n WHERE n.id IN (v_node, w.dst) AND n.retired_at IS NOT NULL)
          AND (SELECT e.close_reason FROM graph.edge e
               WHERE e.account = v_account AND e.src = v_node AND e.rel = w.rel AND e.dst = w.dst
                 AND e.valid_to IS NOT NULL
               ORDER BY e.valid_to DESC, e.id DESC LIMIT 1) IS DISTINCT FROM 'rejected'
        ORDER BY w.rel, w.dst
        RETURNING state
    )
    SELECT count(*), count(*) FILTER (WHERE state = 'proposed') INTO v_opened, v_proposed FROM ins;
    RETURN ARRAY[v_opened, v_closed, v_proposed];
END;
$$;

CREATE FUNCTION public.doc_retire(p_repo text, p_path text) RETURNS integer[]
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_node bigint;
    v_links int;
    v_edges int;
    v_account text := public.instance_account();
BEGIN
    UPDATE public.documents SET retired_at = clock_timestamp(), derived_sha256 = NULL
    WHERE repo = p_repo AND path = p_path AND retired_at IS NULL;
    UPDATE public.ticket_documents SET valid_to = clock_timestamp()
    WHERE repo = p_repo AND path = p_path AND valid_to IS NULL;
    GET DIAGNOSTICS v_links = ROW_COUNT;
    UPDATE public.document_ref SET valid_to = clock_timestamp()
    WHERE repo = p_repo AND path = p_path AND valid_to IS NULL;
    SELECT node_id INTO v_node FROM graph.document WHERE account = v_account AND repo = p_repo AND path = p_path;
    UPDATE graph.edge e SET valid_to = now(), close_reason = 'removed', closed_by = current_setting('hub.actor')
    WHERE e.account = v_account AND (e.src = v_node OR e.dst = v_node) AND e.valid_to IS NULL
      AND e.evidence->>'source' = 'git' AND NOT (e.state = 'confirmed' AND e.confirmed_kind = 'human');
    GET DIAGNOSTICS v_edges = ROW_COUNT;
    RETURN ARRAY[v_links, v_edges];
END;
$$;

CREATE FUNCTION public.doc_role(p_path text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    SELECT CASE WHEN p_path LIKE '%/intent.md' OR split_part(p_path, '/', 3) = 'intents' THEN 'intent'
                WHEN split_part(p_path, '/', 3) = 'specs' THEN 'spec' ELSE 'plan' END
$$;

CREATE FUNCTION public.doc_strip_code(p_body text) RETURNS text
    LANGUAGE plpgsql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    v_lines text[] := string_to_array(replace(p_body, E'\r', ''), E'\n');
    v_n int := coalesce(array_length(v_lines, 1), 0);
    v_line text;
    v_fence text;
    v_open text;
    v_indent boolean := false;
    v_blank boolean := true;
    v_list boolean := false;
    i int := 1;
BEGIN
    IF v_n > 1 AND v_lines[1] = '---' THEN
        FOR j IN 2 .. least(v_n, 200) LOOP
            IF v_lines[j] IN ('---', '...') THEN
                FOR k IN 1 .. j LOOP v_lines[k] := ''; END LOOP;
                i := j + 1;
                EXIT;
            END IF;
        END LOOP;
    END IF;
    WHILE i <= v_n LOOP
        v_line := v_lines[i];
        IF v_fence IS NOT NULL THEN
            IF v_line ~ ('^[ \t]*' || v_fence || '+[ \t]*$') THEN
                v_fence := NULL;
            END IF;
            v_lines[i] := '';
        ELSE
            v_open := substring(v_line from '^[ \t]*(`{3,}|~{3,})');
            IF v_open IS NOT NULL THEN
                -- A fence is closed by the same character, not shorter than the opening one.
                v_fence := repeat(CASE WHEN left(v_open, 1) = '`' THEN '`' ELSE '~' END, length(v_open) - 1)
                           || CASE WHEN left(v_open, 1) = '`' THEN '`' ELSE '~' END;
                v_lines[i] := '';
                v_indent := false;
                v_blank := false;
            ELSIF v_line ~ '^[[:space:]]*$' THEN
                v_blank := true;
            ELSIF v_line ~ '^( {4}|\t)' AND (v_indent OR (v_blank AND NOT v_list)) THEN
                v_indent := true;
                v_lines[i] := '';
            ELSE
                v_indent := false;
                v_list := v_line ~ '^[[:space:]]*([-*+]|[0-9]+[.)])[[:space:]]' OR (v_list AND v_line ~ '^[[:space:]]');
                v_blank := false;
            END IF;
        END IF;
        i := i + 1;
    END LOOP;
    RETURN array_to_string(v_lines, E'\n');
END;
$_$;

CREATE FUNCTION public.doc_ticket_tokens(p_text text, p_labels jsonb) RETURNS TABLE(rel text, token text)
    LANGUAGE plpgsql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    v_lines text[] := string_to_array(p_text, E'\n');
    v_n int := coalesce(array_length(v_lines, 1), 0);
    v_describes text := public.doc_link_label_pattern(p_labels, 'describes');
    v_closes text := public.doc_link_label_pattern(p_labels, 'closes');
    v_and text := public.doc_link_label_pattern(p_labels, 'and');
    v_rest text;
BEGIN
    FOR i IN 1 .. least(v_n, 20) LOOP
        v_rest := substring(v_lines[i] from
            '^\s*(?:\*\*)?' || v_describes || '(?:\*\*)?(?:\s*:(?:\*\*)?\s|\s*(?:\*\*)?\s*(?=`))(.*)$');
        IF v_rest IS NOT NULL THEN
            v_rest := substring(v_rest from '^((?:(?:\s|,|;|\*\*|' || v_and || '\s|\([^()`]*\))*`[^`]+`)*)');
            RETURN QUERY SELECT 'describes'::text, m[1] FROM regexp_matches(coalesce(v_rest, ''), '`([^`]+)`', 'g') AS m;
            EXIT;
        END IF;
    END LOOP;
    RETURN QUERY
        SELECT 'closes'::text, m[1]
        FROM unnest(v_lines) AS l(line), regexp_matches(l.line, '`([^`]+)`', 'g') AS m
        WHERE l.line ~ ('^\s*[-*]?\s*(\*\*)?' || v_closes || '(\*\*)?\s*:');
END;
$_$;

CREATE FUNCTION public.doc_title(p_path text, p_body text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
    SELECT left(coalesce(
        regexp_replace((regexp_match(p_body, '(?n)^#[ \t\f\v]+[^[:space:]][^\r]*'))[1], '^#[ \t\f\v]+', ''),
        regexp_replace(p_path, '^.*/', '')), 300)
$$;

CREATE FUNCTION public.doc_topic(p_path text) RETURNS text
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
    SELECT regexp_replace(regexp_replace(public.doc_name(p_path), '^[0-9]{4}-[0-9]{2}-[0-9]{2}-', ''),
                          '-(design|spec|plan|intent)$', '')
$_$;

CREATE FUNCTION public.doc_valid_path(p_path text) RETURNS boolean
    LANGUAGE sql IMMUTABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
    SELECT p_path ~ '^docs/superpowers/(intents|specs|plans)/([^/[:space:][:cntrl:]]+/)*[^/[:space:][:cntrl:]]+\.md$'
       AND p_path !~ '(^|/)\.\.?(/|$)' AND strpos(p_path, chr(92)) = 0 AND p_path NOT LIKE '%/README.md'
$_$;

CREATE FUNCTION public.edge_change_log() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_who text[] := public.hub_actor();
    v_old jsonb;
    v_new jsonb := jsonb_build_object('rel', NEW.rel, 'src', graph.node_label(NEW.src), 'dst', graph.node_label(NEW.dst),
                                      'state', NEW.state, 'close_reason', NEW.close_reason);
BEGIN
    IF TG_OP = 'UPDATE' THEN
        v_old := jsonb_build_object('rel', OLD.rel, 'src', graph.node_label(OLD.src), 'dst', graph.node_label(OLD.dst),
                                    'state', OLD.state, 'close_reason', OLD.close_reason);
        IF v_old = v_new THEN
            RETURN NULL;
        END IF;
    END IF;
    INSERT INTO public.ticket_change(account, ticket_seq, field, old, new, edge_id, actor, actor_kind)
    SELECT NEW.account, gt.ticket_seq, 'edge:' || NEW.rel, v_old, v_new, NEW.id, v_who[1], v_who[2]
    FROM graph.ticket gt
    WHERE gt.account = NEW.account AND gt.node_id IN (NEW.src, NEW.dst)
    ORDER BY gt.ticket_seq;
    RETURN NULL;
END;
$$;

CREATE FUNCTION public.hub_actor() RETURNS text[]
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_actor text := nullif(current_setting('hub.actor', true), '');
    v_kind text := nullif(current_setting('hub.actor_kind', true), '');
BEGIN
    IF v_actor IS NULL OR v_kind IS NULL THEN
        RAISE EXCEPTION 'hub.actor and hub.actor_kind are required' USING ERRCODE = 'GRACT';
    END IF;
    RETURN ARRAY[v_actor, v_kind];
END;
$$;

CREATE FUNCTION public.import_document_repositories(p_repos jsonb) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    r jsonb;
    v_old public.document_repository;
    v_repo text;
    v_project text;
    v_caller text;
    v_created int := 0;
    v_updated int := 0;
    v_unchanged int := 0;
BEGIN
    IF jsonb_typeof(p_repos) IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'import_document_repositories: a JSON array expected' USING ERRCODE = '22023';
    END IF;
    PERFORM set_config('hub.actor', 'seed:repositories', true), set_config('hub.actor_kind', 'seed', true);
    -- The same lock as the document snapshot: the registry does not change in the middle of a sync, two pods on start
    -- do not import at the same time.
    PERFORM pg_advisory_xact_lock(193700, 8);
    FOR r IN SELECT value FROM jsonb_array_elements(p_repos) LOOP
        IF jsonb_typeof(r -> 'repo') IS DISTINCT FROM 'string' OR jsonb_typeof(r -> 'project') IS DISTINCT FROM 'string'
           OR jsonb_typeof(r -> 'caller') IS DISTINCT FROM 'string' THEN
            RAISE EXCEPTION 'import_document_repositories: repo, project and caller are strings: %', r
                USING ERRCODE = '22023';
        END IF;
        v_repo := r ->> 'repo';
        v_project := r ->> 'project';
        v_caller := r ->> 'caller';
        SELECT * INTO v_old FROM public.document_repository WHERE repo = v_repo FOR UPDATE;
        IF NOT FOUND THEN
            INSERT INTO public.document_repository(repo, project, caller) VALUES (v_repo, v_project, v_caller);
            v_created := v_created + 1;
        ELSIF (v_old.project, v_old.caller) IS DISTINCT FROM (v_project, v_caller) THEN
            UPDATE public.document_repository SET project = v_project, caller = v_caller WHERE repo = v_repo;
            v_updated := v_updated + 1;
        ELSE
            v_unchanged := v_unchanged + 1;
            CONTINUE;
        END IF;
        INSERT INTO graph.node(kind, key) VALUES ('project', v_project) ON CONFLICT DO NOTHING;
    END LOOP;
    RETURN jsonb_build_object('created', v_created, 'updated', v_updated, 'unchanged', v_unchanged);
END;
$$;

CREATE FUNCTION public.instance_account() RETURNS text
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v text;
BEGIN
    SELECT account INTO v FROM public.instance WHERE id;
    IF v IS NULL THEN
        RAISE EXCEPTION 'public.instance has no row: run `hub migrate` with HUB_ACCOUNT and HUB_TIMEZONE'
            USING ERRCODE = 'HUBIN';
    END IF;
    RETURN v;
END;
$$;

CREATE FUNCTION public.instance_account_fixed() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
BEGIN
    IF NEW.account IS DISTINCT FROM OLD.account THEN
        RAISE EXCEPTION 'instance account is %, refusing %: the account is written once', OLD.account, NEW.account
            USING ERRCODE = 'HUBAC';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION public.instance_document_link_labels() RETURNS jsonb
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v jsonb;
BEGIN
    SELECT document_link_labels INTO v FROM public.instance WHERE id;
    IF v IS NULL THEN
        RAISE EXCEPTION 'public.instance has no row: run `hub migrate` with HUB_ACCOUNT and HUB_TIMEZONE'
            USING ERRCODE = 'HUBIN';
    END IF;
    RETURN v;
END;
$$;

CREATE FUNCTION public.instance_timezone() RETURNS text
    LANGUAGE plpgsql STABLE
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v text;
BEGIN
    SELECT timezone INTO v FROM public.instance WHERE id;
    IF v IS NULL THEN
        RAISE EXCEPTION 'public.instance has no row: run `hub migrate` with HUB_ACCOUNT and HUB_TIMEZONE'
            USING ERRCODE = 'HUBIN';
    END IF;
    RETURN v;
END;
$$;

CREATE FUNCTION public.seed_instance(p_account text, p_timezone text, p_pending_limit integer, p_pending_ttl interval, p_document_roots text[], p_document_link_labels jsonb DEFAULT NULL::jsonb) RETURNS text
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_old   text;
    v_done  boolean;
    v_actor text;
    v_kind  text;
BEGIN
    -- Only a name from the zone database: `AT TIME ZONE` would also accept an abbreviation (MSK) or a POSIX offset
    -- (UTC+3, which POSIX reads as UTC-3), and those are not in pg_timezone_names.
    IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_timezone_names WHERE name = p_timezone) THEN
        RAISE EXCEPTION 'unknown time zone %', p_timezone USING ERRCODE = '22023';
    END IF;
    SELECT timezone INTO v_old FROM public.instance WHERE id;
    INSERT INTO public.instance AS i (account, timezone, mailbox_pending_limit, mailbox_pending_ttl, document_roots)
    VALUES (p_account, p_timezone, p_pending_limit, p_pending_ttl, p_document_roots)
    ON CONFLICT (id) DO UPDATE
        SET timezone = EXCLUDED.timezone,
            mailbox_pending_limit = EXCLUDED.mailbox_pending_limit,
            mailbox_pending_ttl = EXCLUDED.mailbox_pending_ttl,
            document_roots = EXCLUDED.document_roots
        WHERE i.account = EXCLUDED.account
    RETURNING true INTO v_done;
    IF v_done IS NULL THEN
        RAISE EXCEPTION 'instance account is %, refusing %: the account is written once',
            (SELECT account FROM public.instance WHERE id), p_account
            USING ERRCODE = 'HUBAC';
    END IF;
    IF p_document_link_labels IS NOT NULL THEN
        UPDATE public.instance SET document_link_labels = p_document_link_labels
        WHERE id AND document_link_labels IS DISTINCT FROM p_document_link_labels;
    END IF;
    -- A new row: the reference data that migrations seeded before it (under the previous default) is copied to the
    -- installation account too. The same account has nothing to copy; a repeat adds nothing.
    IF v_old IS NULL THEN
        INSERT INTO graph.node_kind (account, kind)
        SELECT DISTINCT p_account, k.kind FROM graph.node_kind k WHERE k.account <> p_account
        ON CONFLICT DO NOTHING;
        INSERT INTO graph.rel_shape (account, rel, src_kind, dst_kind, single, acyclic, needs_confirm)
        SELECT DISTINCT ON (s.rel, s.src_kind, s.dst_kind)
               p_account, s.rel, s.src_kind, s.dst_kind, s.single, s.acyclic, s.needs_confirm
        FROM graph.rel_shape s WHERE s.account <> p_account
        ORDER BY s.rel, s.src_kind, s.dst_kind, s.account
        ON CONFLICT DO NOTHING;
        -- Default points are versioned (version_guard): the seed writes them, the caller's actor comes back afterwards.
        v_actor := current_setting('hub.actor', true);
        v_kind := current_setting('hub.actor_kind', true);
        PERFORM set_config('hub.actor', 'seed:instance', true), set_config('hub.actor_kind', 'seed', true);
        INSERT INTO plan.type_default_estimate (account, tag, effort_sp)
        SELECT DISTINCT ON (d.tag) p_account, d.tag, d.effort_sp
        FROM plan.type_default_estimate d
        WHERE d.account <> p_account AND d.valid_to IS NULL
          AND NOT EXISTS (SELECT 1 FROM plan.type_default_estimate x
                          WHERE x.account = p_account AND x.tag = d.tag AND x.valid_to IS NULL)
        ORDER BY d.tag, d.account;
        PERFORM set_config('hub.actor', coalesce(v_actor, ''), true),
                set_config('hub.actor_kind', coalesce(v_kind, ''), true);
    END IF;
    RETURN v_old;
END;
$$;

CREATE FUNCTION public.sync_repository_documents(snapshot jsonb) RETURNS jsonb
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $_$
DECLARE
    v_repo text := snapshot->>'repo';
    v_rev text := snapshot->>'revision';
    v_reg public.document_repository;
    v_kind text;
    v_previous text;
    v_known text[];
    v_doc jsonb;
    v_path text;
    v_body text;
    v_sha text;
    v_role text;
    v_title text;
    v_old public.documents;
    v_ext jsonb;
    v_problems jsonb := '[]'::jsonb;
    v_touched text[] := '{}';
    v_retired text[] := '{}';
    v_reparsed text[] := '{}';
    v_r int[];
    v_n int;
    v_updated int := 0;
    v_restored int := 0;
    v_retires int := 0;
    v_links_opened int := 0;
    v_links_closed int := 0;
    v_edges_opened int := 0;
    v_edges_closed int := 0;
    v_edges_proposed int := 0;
    v_src record;
    v_labels jsonb;
BEGIN
    PERFORM pg_advisory_xact_lock(193700, 8);
    -- A write through kubectl exec (without hub) comes without an actor: during the transition it is service:docs-exec.
    IF nullif(current_setting('hub.actor', true), '') IS NULL
       OR nullif(current_setting('hub.actor_kind', true), '') IS NULL THEN
        PERFORM set_config('hub.actor', 'docs-exec', true), set_config('hub.actor_kind', 'service:docs-exec', true);
    END IF;
    v_kind := current_setting('hub.actor_kind');

    SELECT * INTO v_reg FROM public.document_repository WHERE repo = v_repo;
    IF jsonb_typeof(snapshot->'repo') IS DISTINCT FROM 'string' OR v_reg IS NULL THEN
        RAISE EXCEPTION 'unsupported document repository: %', v_repo USING ERRCODE = 'DSREP';
    END IF;
    IF v_kind LIKE 'service:docs-%' AND v_kind <> 'service:docs-exec' AND v_kind <> 'service:' || v_reg.caller THEN
        RAISE EXCEPTION 'caller % does not own %', v_kind, v_repo USING ERRCODE = 'DSCAL';
    END IF;
    IF jsonb_typeof(snapshot->'revision') IS DISTINCT FROM 'string' OR v_rev !~ '^[a-f0-9]{40}$'
       OR jsonb_typeof(snapshot->'documents') IS DISTINCT FROM 'array'
       OR jsonb_typeof(snapshot->'ancestors') IS DISTINCT FROM 'array' THEN
        RAISE EXCEPTION 'invalid document snapshot' USING ERRCODE = 'DSINV';
    END IF;
    IF NOT (snapshot->'ancestors' ? v_rev) OR EXISTS (
        SELECT 1 FROM jsonb_array_elements(snapshot->'ancestors') AS a(value)
        WHERE jsonb_typeof(value) <> 'string' OR value #>> '{}' !~ '^[a-f0-9]{40}$'
    ) THEN
        RAISE EXCEPTION 'invalid document ancestry' USING ERRCODE = 'DSINV';
    END IF;
    IF jsonb_array_length(snapshot->'documents') > 5000 THEN
        RAISE EXCEPTION 'invalid document snapshot: more than 5000 documents' USING ERRCODE = 'DSINV';
    END IF;
    IF EXISTS (
        SELECT 1 FROM jsonb_array_elements(snapshot->'documents') AS d(value)
        WHERE jsonb_typeof(value) <> 'object'
    ) THEN
        RAISE EXCEPTION 'invalid document snapshot: a document is not an object' USING ERRCODE = 'DSINV';
    END IF;
    IF EXISTS (
        SELECT 1 FROM jsonb_array_elements(snapshot->'documents') AS d(value)
        WHERE jsonb_typeof(value->'path') = 'string' GROUP BY value->>'path' HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'invalid document snapshot: duplicate document path' USING ERRCODE = 'DSINV';
    END IF;
    SELECT revision INTO v_previous FROM public.repository_document_sync_state WHERE repo = v_repo;
    IF v_previous IS NOT NULL AND v_previous <> v_rev AND NOT (snapshot->'ancestors' ? v_previous) THEN
        RAISE EXCEPTION 'stale or unrelated document snapshot: % after %', v_rev, v_previous USING ERRCODE = 'DSSTL';
    END IF;

    -- Link labels of the installation (0027), read once per snapshot.
    v_labels := public.instance_document_link_labels();

    v_known := ARRAY(SELECT value->>'path' FROM jsonb_array_elements(snapshot->'documents') AS d(value)
                     WHERE jsonb_typeof(value->'path') = 'string');

    FOR v_doc IN SELECT value FROM jsonb_array_elements(snapshot->'documents') LOOP
        v_path := CASE WHEN jsonb_typeof(v_doc->'path') = 'string' THEN v_doc->>'path' END;
        IF v_path IS NULL OR NOT public.doc_valid_path(v_path) THEN
            v_problems := v_problems || jsonb_build_object('path', left(coalesce(v_path, (v_doc->'path')::text, ''), 300),
                                                           'kind', 'bad_path', 'detail', '');
            CONTINUE;
        END IF;
        IF jsonb_typeof(v_doc->'body') IS DISTINCT FROM 'string' THEN
            -- The old collector does not send a body over 1 MiB (body: null, bytes: the size).
            v_problems := v_problems || jsonb_build_object('path', v_path,
                'kind', CASE WHEN jsonb_typeof(v_doc->'bytes') = 'number' AND (v_doc->>'bytes')::numeric > 1048576
                             THEN 'too_large' ELSE 'bad_body' END, 'detail', coalesce(v_doc->>'bytes', ''));
            CONTINUE;
        END IF;
        v_body := v_doc->>'body';
        IF octet_length(convert_to(v_body, 'UTF8')) > 1048576 THEN
            v_problems := v_problems || jsonb_build_object('path', v_path, 'kind', 'too_large',
                                                           'detail', octet_length(convert_to(v_body, 'UTF8'))::text);
            CONTINUE;
        END IF;
        v_sha := encode(sha256(convert_to(v_body, 'UTF8')), 'hex');
        v_role := public.doc_role(v_path);
        v_title := public.doc_title(v_path, v_body);
        IF (v_doc ? 'role' AND v_doc->>'role' IS DISTINCT FROM v_role)
           OR (v_doc ? 'sha' AND v_doc->>'sha' IS DISTINCT FROM v_sha)
           OR (v_doc ? 'bytes' AND (jsonb_typeof(v_doc->'bytes') IS DISTINCT FROM 'number'
                                    OR (v_doc->>'bytes')::numeric <> octet_length(convert_to(v_body, 'UTF8')))) THEN
            v_problems := v_problems || jsonb_build_object('path', v_path, 'kind', 'bad_meta',
                                                           'detail', 'role, sha or bytes differ from the body');
            CONTINUE;
        END IF;

        SELECT * INTO v_old FROM public.documents WHERE repo = v_repo AND path = v_path;
        -- Unchanged: no parsing needed. A document with a problem in its previous parse (derived_sha256 NULL) is parsed
        -- again, so that problems are neither lost nor stale; the limit of 200 entries does not affect this.
        CONTINUE WHEN v_old.path IS NOT NULL AND v_old.sha256 = v_sha AND v_old.role = v_role
            AND v_old.title = v_title AND v_old.retired_at IS NULL AND v_old.derived_sha256 = v_sha;

        v_ext := public.doc_extract(v_repo, v_path, v_body, v_known, v_labels);
        IF jsonb_array_length(v_ext->'problems') > 0 THEN
            v_problems := v_problems || (SELECT jsonb_agg(p || jsonb_build_object('path', v_path))
                                         FROM jsonb_array_elements(v_ext->'problems') AS p);
            -- A dangling link does not stop the document from syncing; an unknown code or too many codes do.
            CONTINUE WHEN EXISTS (SELECT 1 FROM jsonb_array_elements(v_ext->'problems') AS p
                                  WHERE p->>'kind' <> 'dangling_link');
        END IF;

        INSERT INTO public.documents(repo, path, role, title, body, sha256, source_revision, created_at, synced_at)
        VALUES (v_repo, v_path, v_role, v_title, v_body, v_sha, v_rev, clock_timestamp(), clock_timestamp())
        ON CONFLICT (repo, path) DO UPDATE SET
            role = excluded.role, title = excluded.title, body = excluded.body, sha256 = excluded.sha256,
            source_revision = excluded.source_revision, synced_at = excluded.synced_at, retired_at = NULL
        WHERE (documents.role, documents.title, documents.sha256, documents.retired_at)
           IS DISTINCT FROM (excluded.role, excluded.title, excluded.sha256, NULL::timestamptz);
        GET DIAGNOSTICS v_n = ROW_COUNT;
        v_updated := v_updated + v_n;
        IF v_old.retired_at IS NOT NULL THEN
            v_restored := v_restored + 1;
        END IF;
        -- The first parse of an unchanged text (a document imported from the old system) only adds links, like the backfill.
        v_r := public.doc_apply(v_repo, v_path, v_ext,
                                NOT (v_old.sha256 IS NOT DISTINCT FROM v_sha AND v_old.derived_sha256 IS NULL
                                     AND v_old.retired_at IS NULL));
        v_links_opened := v_links_opened + v_r[1];
        v_links_closed := v_links_closed + v_r[2];
        v_touched := v_touched || v_path;
    END LOOP;

    FOR v_path IN
        SELECT d.path FROM public.documents d
        WHERE d.repo = v_repo AND d.retired_at IS NULL AND NOT (d.path = ANY (v_known))
        ORDER BY d.path
    LOOP
        v_r := public.doc_retire(v_repo, v_path);
        v_retires := v_retires + 1;
        v_links_closed := v_links_closed + v_r[1];
        v_edges_closed := v_edges_closed + v_r[2];
        v_retired := v_retired || v_path;
    END LOOP;

    -- A bare path is resolved against the live documents of all repos (doc_extract), so it depends on which repos are
    -- already synced. So that the sync order of repos does not matter, live documents of any repo with a live link to a
    -- path that has just appeared or been retired (with any target_repo) are parsed again from the stored text: the own
    -- repo against its snapshot, another repo against its live documents (without the dangling link check and without
    -- marking derived_sha256: the sync of that repo owns them).
    v_reparsed := ARRAY(
        SELECT DISTINCT r.repo || E'\n' || r.path FROM public.document_ref r
        JOIN public.documents s ON s.repo = r.repo AND s.path = r.path AND s.retired_at IS NULL
        WHERE r.valid_to IS NULL AND r.target_path = ANY (v_touched || v_retired)
          AND NOT (r.repo = v_repo AND r.path = ANY (v_touched))
        ORDER BY 1);
    FOR v_src IN
        SELECT d.repo, d.path, d.body, d.sha256, d.derived_sha256 FROM public.documents d
        WHERE d.repo || E'\n' || d.path = ANY (v_reparsed) ORDER BY d.repo, d.path
    LOOP
        v_ext := public.doc_extract(v_src.repo, v_src.path, v_src.body,
                                    CASE WHEN v_src.repo = v_repo THEN v_known END, v_labels);
        IF v_src.repo = v_repo AND jsonb_array_length(v_ext->'problems') > 0 THEN
            v_problems := v_problems || (SELECT jsonb_agg(p || jsonb_build_object('path', v_src.path))
                                         FROM jsonb_array_elements(v_ext->'problems') AS p);
        END IF;
        v_r := public.doc_apply(v_src.repo, v_src.path, v_ext, v_src.derived_sha256 IS NOT DISTINCT FROM v_src.sha256,
                                v_src.repo = v_repo);
        v_links_opened := v_links_opened + v_r[1];
        v_links_closed := v_links_closed + v_r[2];
    END LOOP;

    -- Edges: changed and restored documents, live documents of their topics and of the retired ones (the topic rule
    -- depends on the neighbours), the ones parsed again above and documents of any repo with a live link to the changed
    -- ones (the target may have appeared only now).
    IF cardinality(v_touched) + cardinality(v_retired) > 0 THEN
        FOR v_src IN
            SELECT DISTINCT x.repo, x.path FROM (
                SELECT v_repo AS repo, t AS path FROM unnest(v_touched) AS t
                UNION ALL
                SELECT split_part(t, E'\n', 1), split_part(t, E'\n', 2) FROM unnest(v_reparsed) AS t
                UNION ALL
                SELECT d.repo, d.path FROM public.documents d
                WHERE d.repo = v_repo AND d.retired_at IS NULL
                  AND public.doc_topic(d.path) IN (SELECT public.doc_topic(t) FROM unnest(v_touched || v_retired) AS t)
                UNION ALL
                SELECT r.repo, r.path FROM public.document_ref r
                JOIN public.documents s ON s.repo = r.repo AND s.path = r.path AND s.retired_at IS NULL
                WHERE r.valid_to IS NULL AND r.target_repo = v_repo AND r.target_path = ANY (v_touched)
            ) x ORDER BY x.repo, x.path
        LOOP
            v_r := public.doc_reconcile(v_src.repo, v_src.path, v_rev);
            v_edges_opened := v_edges_opened + v_r[1];
            v_edges_closed := v_edges_closed + v_r[2];
            v_edges_proposed := v_edges_proposed + v_r[3];
        END LOOP;
    END IF;

    v_problems := coalesce((SELECT jsonb_agg(p ORDER BY n) FROM jsonb_array_elements(v_problems) WITH ORDINALITY AS x(p, n)
                            WHERE n <= 200), '[]');
    INSERT INTO public.repository_document_sync_state(repo, revision, synced_at, problems)
    VALUES (v_repo, v_rev, clock_timestamp(), v_problems)
    ON CONFLICT (repo) DO UPDATE SET revision = excluded.revision, synced_at = excluded.synced_at,
                                     problems = excluded.problems
    WHERE (repository_document_sync_state.revision, repository_document_sync_state.problems)
          IS DISTINCT FROM (excluded.revision, excluded.problems);
    RETURN jsonb_build_object(
        'repo', v_repo, 'revision', v_rev, 'documents', jsonb_array_length(snapshot->'documents'),
        'updated', v_updated, 'retired', v_retires, 'restored', v_restored,
        'links', jsonb_build_object('opened', v_links_opened, 'closed', v_links_closed),
        'edges', jsonb_build_object('opened', v_edges_opened, 'closed', v_edges_closed, 'proposed', v_edges_proposed),
        'problems', v_problems);
END;
$_$;

CREATE FUNCTION public.ticket_change_log() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_who text[] := public.hub_actor();
BEGIN
    INSERT INTO public.ticket_change(ticket_seq, field, old, new, actor, actor_kind)
    SELECT NEW.seq, c.field, c.old, c.new, v_who[1], v_who[2]
    FROM (VALUES
        ('project', to_jsonb(OLD.project), to_jsonb(NEW.project)),
        ('summary', to_jsonb(OLD.summary), to_jsonb(NEW.summary)),
        ('status', to_jsonb(OLD.status), to_jsonb(NEW.status)),
        ('priority', to_jsonb(OLD.priority), to_jsonb(NEW.priority)),
        ('tags', to_jsonb(OLD.tags), to_jsonb(NEW.tags)),
        ('transcript_ref', to_jsonb(OLD.transcript_ref), to_jsonb(NEW.transcript_ref)),
        ('body', jsonb_build_object('bytes', octet_length(OLD.body), 'md5', md5(OLD.body)),
                 jsonb_build_object('bytes', octet_length(NEW.body), 'md5', md5(NEW.body)))
    ) AS c(field, old, new)
    WHERE c.old IS DISTINCT FROM c.new;
    RETURN NULL;
END;
$$;

CREATE FUNCTION public.ticket_create_log() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'pg_temp'
    AS $$
DECLARE
    v_who text[] := public.hub_actor();
BEGIN
    INSERT INTO public.ticket_change(ticket_seq, field, old, new, actor, actor_kind)
    VALUES (NEW.seq, 'created', NULL,
            jsonb_build_object('code', NEW.code, 'project', NEW.project, 'status', NEW.status,
                               'priority', NEW.priority),
            v_who[1], v_who[2]);
    RETURN NULL;
END;
$$;

CREATE TABLE auth.account (
    id text NOT NULL,
    "accountId" text NOT NULL,
    "providerId" text NOT NULL,
    "userId" text NOT NULL,
    "accessToken" text,
    "refreshToken" text,
    "idToken" text,
    "accessTokenExpiresAt" timestamp with time zone,
    "refreshTokenExpiresAt" timestamp with time zone,
    scope text,
    password text,
    "createdAt" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL
);

CREATE TABLE auth.jwks (
    id text NOT NULL,
    "publicKey" text NOT NULL,
    "privateKey" text NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "expiresAt" timestamp with time zone,
    alg text,
    crv text
);

CREATE TABLE auth."oauthAccessToken" (
    id text NOT NULL,
    token text NOT NULL,
    "clientId" text NOT NULL,
    "sessionId" text,
    "userId" text,
    "referenceId" text,
    "authorizationCodeId" text,
    resources jsonb,
    "requestedUserInfoClaims" jsonb,
    "refreshId" text,
    "expiresAt" timestamp with time zone NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    revoked timestamp with time zone,
    confirmation jsonb,
    scopes jsonb NOT NULL
);

CREATE TABLE auth."oauthClient" (
    id text NOT NULL,
    "clientId" text NOT NULL,
    "clientSecret" text,
    "clientDiscoveryId" text,
    disabled boolean,
    "skipConsent" boolean,
    "enableEndSession" boolean,
    "subjectType" text,
    scopes jsonb,
    "clientCredentialsScopes" jsonb,
    "userId" text,
    "createdAt" timestamp with time zone,
    "updatedAt" timestamp with time zone,
    name text,
    uri text,
    icon text,
    contacts jsonb,
    tos text,
    policy text,
    "softwareId" text,
    "softwareVersion" text,
    "softwareStatement" text,
    "redirectUris" jsonb NOT NULL,
    "postLogoutRedirectUris" jsonb,
    "backchannelLogoutUri" text,
    "backchannelLogoutSessionRequired" boolean,
    "tokenEndpointAuthMethod" text,
    "applicationType" text,
    jwks text,
    "jwksUri" text,
    "grantTypes" jsonb,
    "responseTypes" jsonb,
    "requirePKCE" boolean,
    "dpopBoundAccessTokens" boolean,
    "referenceId" text,
    metadata jsonb
);

CREATE TABLE auth."oauthClientAssertion" (
    id text NOT NULL,
    "expiresAt" timestamp with time zone NOT NULL
);

CREATE TABLE auth."oauthClientResource" (
    id text NOT NULL,
    "clientId" text NOT NULL,
    "resourceId" text NOT NULL,
    metadata jsonb,
    "createdAt" timestamp with time zone
);

CREATE TABLE auth."oauthConsent" (
    id text NOT NULL,
    "clientId" text NOT NULL,
    "userId" text,
    "referenceId" text,
    resources jsonb,
    "requestedUserInfoClaims" jsonb,
    scopes jsonb NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL
);

CREATE TABLE auth."oauthRefreshToken" (
    id text NOT NULL,
    token text NOT NULL,
    "clientId" text NOT NULL,
    "sessionId" text,
    "userId" text NOT NULL,
    "referenceId" text,
    "authorizationCodeId" text,
    resources jsonb,
    "requestedUserInfoClaims" jsonb,
    "expiresAt" timestamp with time zone NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    revoked timestamp with time zone,
    "rotatedAt" timestamp with time zone,
    "rotationReplayResponse" text,
    "rotationReplayExpiresAt" timestamp with time zone,
    "authTime" timestamp with time zone,
    confirmation jsonb,
    scopes jsonb NOT NULL
);

CREATE TABLE auth."oauthResource" (
    id text NOT NULL,
    identifier text NOT NULL,
    name text NOT NULL,
    "accessTokenTtl" integer,
    "refreshTokenTtl" integer,
    "signingAlgorithm" text,
    "signingKeyId" text,
    "allowedScopes" jsonb,
    "customClaims" jsonb,
    "dpopBoundAccessTokensRequired" boolean,
    disabled boolean,
    "createdAt" timestamp with time zone,
    "updatedAt" timestamp with time zone,
    "policyVersion" integer,
    metadata jsonb
);

CREATE TABLE auth.session (
    id text NOT NULL,
    "expiresAt" timestamp with time zone NOT NULL,
    token text NOT NULL,
    "createdAt" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    "ipAddress" text,
    "userAgent" text,
    "userId" text NOT NULL
);

CREATE TABLE auth."user" (
    id text NOT NULL,
    name text NOT NULL,
    email text NOT NULL,
    "emailVerified" boolean NOT NULL,
    image text,
    "createdAt" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    "updatedAt" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE auth.verification (
    id text NOT NULL,
    identifier text NOT NULL,
    value text NOT NULL,
    "expiresAt" timestamp with time zone NOT NULL,
    "createdAt" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL,
    "updatedAt" timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

CREATE TABLE graph.component (
    node_id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    kind text DEFAULT 'component'::text NOT NULL,
    slug text NOT NULL,
    title text NOT NULL,
    description text,
    CONSTRAINT component_description_check CHECK (((description IS NULL) OR (length(description) <= 2000))),
    CONSTRAINT component_kind_check CHECK ((kind = 'component'::text)),
    CONSTRAINT component_slug_check CHECK ((slug ~ '^[a-z0-9][a-z0-9-]{0,39}$'::text)),
    CONSTRAINT component_title_check CHECK (((length(title) >= 1) AND (length(title) <= 200)))
);

CREATE TABLE graph.document (
    node_id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    kind text DEFAULT 'document'::text NOT NULL,
    repo text NOT NULL,
    path text NOT NULL,
    CONSTRAINT document_kind_check CHECK ((kind = 'document'::text))
);

CREATE TABLE graph.edge (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    src bigint NOT NULL,
    src_kind text NOT NULL,
    rel text NOT NULL,
    dst bigint NOT NULL,
    dst_kind text NOT NULL,
    state text DEFAULT 'confirmed'::text NOT NULL,
    evidence jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    created_by text DEFAULT ''::text NOT NULL,
    confirmed_at timestamp with time zone,
    confirmed_by text,
    confirmed_kind text,
    valid_to timestamp with time zone,
    closed_by text,
    close_reason text,
    CONSTRAINT edge_check CHECK ((src <> dst)),
    CONSTRAINT edge_check1 CHECK (((valid_to IS NULL) = (close_reason IS NULL))),
    CONSTRAINT edge_check2 CHECK (((valid_to IS NULL) = (closed_by IS NULL))),
    CONSTRAINT edge_check3 CHECK (((state = 'confirmed'::text) = (confirmed_at IS NOT NULL))),
    CONSTRAINT edge_check4 CHECK (((confirmed_at IS NULL) = (confirmed_by IS NULL))),
    CONSTRAINT edge_check5 CHECK (((confirmed_at IS NULL) = (confirmed_kind IS NULL))),
    CONSTRAINT edge_close_reason_check CHECK ((close_reason = ANY (ARRAY['removed'::text, 'rejected'::text, 'superseded'::text, 'node_retired'::text]))),
    CONSTRAINT edge_created_by_check CHECK (((length(created_by) >= 1) AND (length(created_by) <= 200))),
    CONSTRAINT edge_evidence_check CHECK (((evidence IS NULL) OR (jsonb_typeof(evidence) = 'object'::text))),
    CONSTRAINT edge_state_check CHECK ((state = ANY (ARRAY['proposed'::text, 'confirmed'::text])))
);

ALTER TABLE graph.edge ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME graph.edge_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE graph.node (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    kind text NOT NULL,
    key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    retired_at timestamp with time zone,
    retire_reason text,
    CONSTRAINT node_check CHECK (((length(key) >= 1) AND ((kind = 'document'::text) OR (length(key) <= 300)))),
    CONSTRAINT node_check1 CHECK (((retired_at IS NULL) = (retire_reason IS NULL)))
);

CREATE TABLE graph.ticket (
    node_id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    kind text DEFAULT 'ticket'::text NOT NULL,
    ticket_seq bigint NOT NULL,
    CONSTRAINT ticket_kind_check CHECK ((kind = 'ticket'::text))
);

CREATE TABLE public.ticket_documents (
    ticket_seq bigint NOT NULL,
    repo text NOT NULL,
    path text NOT NULL,
    rel text NOT NULL,
    valid_from timestamp with time zone NOT NULL,
    valid_to timestamp with time zone,
    CONSTRAINT ticket_documents_rel CHECK ((rel = ANY (ARRAY['describes'::text, 'closes'::text])))
);

CREATE TABLE public.tickets (
    seq bigint NOT NULL,
    code text NOT NULL,
    summary text NOT NULL,
    status text DEFAULT 'open'::text NOT NULL,
    body text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    project text NOT NULL,
    priority text,
    tags text[] DEFAULT '{}'::text[] NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    transcript_ref text,
    legacy_id text,
    imported_updated_at timestamp with time zone,
    CONSTRAINT tickets_priority_check CHECK ((priority = ANY (ARRAY['p0'::text, 'p1'::text, 'p2'::text, 'p3'::text]))),
    CONSTRAINT tickets_project_slug CHECK ((project ~ '^[a-z0-9][a-z0-9-]{0,39}$'::text)),
    CONSTRAINT tickets_status CHECK ((status = ANY (ARRAY['open'::text, 'doing'::text, 'done'::text, 'cancelled'::text])))
);

CREATE VIEW graph.edge_v AS
 SELECT e.account,
    e.id AS edge_id,
    e.src,
    e.src_kind,
    e.rel,
    e.dst,
    e.dst_kind,
    e.state,
    'edge'::text AS source
   FROM graph.edge e
  WHERE (e.valid_to IS NULL)
UNION ALL
 SELECT gt.account,
    NULL::bigint AS edge_id,
    gt.node_id AS src,
    'ticket'::text AS src_kind,
    'in_project'::text AS rel,
    p.id AS dst,
    'project'::text AS dst_kind,
    'confirmed'::text AS state,
    'tickets.project'::text AS source
   FROM ((public.tickets t
     JOIN graph.ticket gt ON ((gt.ticket_seq = t.seq)))
     JOIN graph.node p ON (((p.account = gt.account) AND (p.kind = 'project'::text) AND (p.key = t.project))))
UNION ALL
 SELECT gd.account,
    NULL::bigint AS edge_id,
    gd.node_id AS src,
    'document'::text AS src_kind,
    l.rel,
    gt.node_id AS dst,
    'ticket'::text AS dst_kind,
    'confirmed'::text AS state,
    'ticket_documents'::text AS source
   FROM ((public.ticket_documents l
     JOIN graph.document gd ON (((gd.repo = l.repo) AND (gd.path = l.path))))
     JOIN graph.ticket gt ON (((gt.ticket_seq = l.ticket_seq) AND (gt.account = gd.account))))
  WHERE (l.valid_to IS NULL);

ALTER TABLE graph.node ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME graph.node_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE graph.node_kind (
    account text DEFAULT public.instance_account() NOT NULL,
    kind text NOT NULL,
    CONSTRAINT node_kind_account_check CHECK ((account ~ '^[a-z][a-z0-9-]{0,39}$'::text)),
    CONSTRAINT node_kind_kind_check CHECK ((kind ~ '^[a-z][a-z_]{1,39}$'::text))
);

CREATE TABLE graph.person (
    node_id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    kind text DEFAULT 'person'::text NOT NULL,
    handle text NOT NULL,
    name text NOT NULL,
    email text,
    aliases text[] DEFAULT '{}'::text[] NOT NULL,
    is_agent boolean DEFAULT false NOT NULL,
    CONSTRAINT person_aliases_check CHECK ((cardinality(aliases) <= 50)),
    CONSTRAINT person_email_check CHECK (((email IS NULL) OR ((length(email) <= 320) AND (email ~ '^[^@[:space:]]+@[^@[:space:]]+$'::text)))),
    CONSTRAINT person_handle_check CHECK (((handle ~ '^[a-z][a-z0-9-]{0,39}$'::text) AND (handle <> 'none'::text))),
    CONSTRAINT person_kind_check CHECK ((kind = 'person'::text)),
    CONSTRAINT person_name_check CHECK (((length(name) >= 1) AND (length(name) <= 200)))
);

CREATE TABLE graph.person_login (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    user_id text NOT NULL,
    handle text NOT NULL,
    actor text DEFAULT ''::text NOT NULL,
    actor_kind text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    retired_at timestamp with time zone,
    retired_by text,
    CONSTRAINT person_login_actor_kind_check CHECK ((actor_kind = 'human'::text)),
    CONSTRAINT person_login_check CHECK (((retired_at IS NULL) = (retired_by IS NULL))),
    CONSTRAINT person_login_user_id_check CHECK (length(user_id) BETWEEN 1 AND 200 AND user_id !~ '[[:cntrl:]]')
);

ALTER TABLE graph.person_login ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME graph.person_login_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE graph.rel_shape (
    account text DEFAULT public.instance_account() NOT NULL,
    rel text NOT NULL,
    src_kind text NOT NULL,
    dst_kind text NOT NULL,
    single boolean DEFAULT false NOT NULL,
    acyclic boolean DEFAULT false NOT NULL,
    needs_confirm boolean DEFAULT false NOT NULL,
    CONSTRAINT rel_shape_rel_check CHECK ((rel ~ '^[a-z][a-z_]{1,39}$'::text))
);

CREATE TABLE mailbox.cursors (
    key_id text NOT NULL,
    agent text NOT NULL,
    last_id bigint DEFAULT 0 NOT NULL,
    pending_id bigint DEFAULT 0 NOT NULL,
    pending_at timestamp with time zone DEFAULT '1970-01-01 00:00:00+00'::timestamp with time zone NOT NULL,
    last_seen_at timestamp with time zone NOT NULL
);

CREATE TABLE mailbox.imports (
    source text NOT NULL,
    imported_at timestamp with time zone NOT NULL,
    keys integer NOT NULL,
    cursors integer NOT NULL,
    messages integer NOT NULL,
    max_id bigint NOT NULL,
    CONSTRAINT imports_source_check CHECK ((source <> ''::text))
);

CREATE TABLE mailbox.key_review (
    id bigint NOT NULL,
    key_id text NOT NULL,
    status text NOT NULL,
    owner text NOT NULL,
    actor text NOT NULL,
    actor_kind text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT key_review_actor_kind_check CHECK ((actor_kind = 'human'::text)),
    CONSTRAINT key_review_status_check CHECK ((status = ANY (ARRAY['approved'::text, 'revoked'::text])))
);

ALTER TABLE mailbox.key_review ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME mailbox.key_review_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE mailbox.messages (
    id bigint NOT NULL,
    key_id text NOT NULL,
    agent text NOT NULL,
    text text NOT NULL,
    created_at timestamp with time zone NOT NULL
);

ALTER TABLE mailbox.messages ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY (
    SEQUENCE NAME mailbox.messages_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE plan.baseline (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    root_seq bigint NOT NULL,
    supersedes bigint,
    scope_id bigint NOT NULL,
    approved_at timestamp with time zone DEFAULT now() NOT NULL,
    approved_by text DEFAULT ''::text NOT NULL,
    approved_kind text DEFAULT ''::text NOT NULL,
    accepted_defaults boolean DEFAULT false NOT NULL,
    defaulted_pct integer NOT NULL,
    max_defaulted_pct integer DEFAULT 0 NOT NULL,
    algo_version integer NOT NULL,
    result jsonb NOT NULL,
    CONSTRAINT baseline_check CHECK (((supersedes IS NULL) OR (supersedes < id))),
    CONSTRAINT baseline_check1 CHECK ((accepted_defaults OR (defaulted_pct <= max_defaulted_pct))),
    CONSTRAINT baseline_defaulted_pct_check CHECK (((defaulted_pct >= 0) AND (defaulted_pct <= 100))),
    CONSTRAINT baseline_result_check CHECK ((jsonb_typeof(result) = 'object'::text))
);

ALTER TABLE plan.baseline ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME plan.baseline_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE VIEW plan.baseline_live AS
 SELECT id,
    account,
    root_seq,
    supersedes,
    scope_id,
    approved_at,
    approved_by,
    approved_kind,
    accepted_defaults,
    defaulted_pct,
    max_defaulted_pct,
    algo_version,
    result
   FROM plan.baseline b
  WHERE (NOT (EXISTS ( SELECT 1
           FROM plan.baseline n
          WHERE ((n.account = b.account) AND (n.supersedes = b.id)))));

CREATE TABLE plan.current (
    account text DEFAULT public.instance_account() NOT NULL,
    root_seq bigint NOT NULL,
    input_hash bytea NOT NULL,
    algo_version integer NOT NULL,
    computed_at timestamp with time zone DEFAULT now() NOT NULL,
    today date NOT NULL,
    baseline_id bigint,
    result jsonb NOT NULL,
    actor text DEFAULT ''::text NOT NULL,
    CONSTRAINT current_input_hash_check CHECK ((octet_length(input_hash) = 32)),
    CONSTRAINT current_result_check CHECK ((jsonb_typeof(result) = 'object'::text))
);

CREATE TABLE plan.dirty (
    account text DEFAULT public.instance_account() NOT NULL,
    root_seq bigint NOT NULL,
    marked bigint DEFAULT 1 NOT NULL,
    done bigint DEFAULT 0 NOT NULL,
    since timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT dirty_check CHECK (((0 <= done) AND (done <= marked)))
);

CREATE TABLE plan.fever_point (
    account text DEFAULT public.instance_account() NOT NULL,
    root_seq bigint NOT NULL,
    day date NOT NULL,
    buffer_key text NOT NULL,
    baseline_id bigint NOT NULL,
    kind text NOT NULL,
    chain_complete_pct integer NOT NULL,
    consumed_pct integer NOT NULL,
    zone text,
    flags text[] DEFAULT '{}'::text[] NOT NULL,
    actor text DEFAULT ''::text NOT NULL,
    CONSTRAINT fever_point_buffer_key_check CHECK ((buffer_key ~ '^(pb|fb:(finish|[0-9]{1,19}))$'::text)),
    CONSTRAINT fever_point_kind_check CHECK ((kind = ANY (ARRAY['project'::text, 'feeding'::text]))),
    CONSTRAINT fever_point_zone_check CHECK ((zone = ANY (ARRAY['green'::text, 'yellow'::text, 'red'::text])))
);

CREATE TABLE plan.points_to_hd (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    effort_sp integer NOT NULL,
    aggressive_hd integer NOT NULL,
    valid_from timestamp with time zone DEFAULT now() NOT NULL,
    valid_to timestamp with time zone,
    actor text DEFAULT ''::text NOT NULL,
    actor_kind text DEFAULT ''::text NOT NULL,
    closed_by text,
    CONSTRAINT points_to_hd_aggressive_hd_check CHECK (((aggressive_hd >= 1) AND (aggressive_hd <= 1000))),
    CONSTRAINT points_to_hd_check CHECK (((valid_to IS NULL) = (closed_by IS NULL))),
    CONSTRAINT points_to_hd_effort_sp_check CHECK ((effort_sp = ANY (ARRAY[1, 2, 3, 5, 8, 13, 21])))
);

ALTER TABLE plan.points_to_hd ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME plan.points_to_hd_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE plan.scope (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    root_seq bigint NOT NULL,
    due_date date,
    mode text DEFAULT 'forward'::text NOT NULL,
    buffer_method text DEFAULT 'cnp'::text NOT NULL,
    green_a_pct integer DEFAULT 10 NOT NULL,
    red_a_pct integer DEFAULT 30 NOT NULL,
    zone_slope_pct integer DEFAULT 60 NOT NULL,
    max_defaulted_pct integer DEFAULT 20 NOT NULL,
    valid_from timestamp with time zone DEFAULT now() NOT NULL,
    valid_to timestamp with time zone,
    actor text DEFAULT ''::text NOT NULL,
    actor_kind text DEFAULT ''::text NOT NULL,
    closed_by text,
    CONSTRAINT scope_buffer_method_check CHECK ((buffer_method = ANY (ARRAY['cnp'::text, 'ssq'::text]))),
    CONSTRAINT scope_check CHECK (((0 <= green_a_pct) AND (green_a_pct <= red_a_pct) AND (red_a_pct <= 1000))),
    CONSTRAINT scope_check1 CHECK (((mode = 'forward'::text) OR (due_date IS NOT NULL))),
    CONSTRAINT scope_check2 CHECK (((valid_to IS NULL) = (closed_by IS NULL))),
    CONSTRAINT scope_max_defaulted_pct_check CHECK (((max_defaulted_pct >= 0) AND (max_defaulted_pct <= 100))),
    CONSTRAINT scope_mode_check CHECK ((mode = ANY (ARRAY['forward'::text, 'backward'::text]))),
    CONSTRAINT scope_zone_slope_pct_check CHECK (((zone_slope_pct >= 0) AND (zone_slope_pct <= 1000)))
);

ALTER TABLE plan.scope ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME plan.scope_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE plan.ticket_estimate (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    ticket_seq bigint NOT NULL,
    effort_sp integer NOT NULL,
    aggressive_hd integer,
    safe_hd integer,
    remaining_hd integer,
    start_not_before date,
    valid_from timestamp with time zone DEFAULT now() NOT NULL,
    valid_to timestamp with time zone,
    actor text DEFAULT ''::text NOT NULL,
    actor_kind text DEFAULT ''::text NOT NULL,
    closed_by text,
    CONSTRAINT ticket_estimate_aggressive_hd_check CHECK (((aggressive_hd >= 1) AND (aggressive_hd <= 1000))),
    CONSTRAINT ticket_estimate_check CHECK (safe_hd BETWEEN 1 AND 2000 AND safe_hd >= aggressive_hd),
    CONSTRAINT ticket_estimate_check1 CHECK (((valid_to IS NULL) = (closed_by IS NULL))),
    CONSTRAINT ticket_estimate_effort_sp_check CHECK ((effort_sp = ANY (ARRAY[1, 2, 3, 5, 8, 13, 21]))),
    CONSTRAINT ticket_estimate_remaining_hd_check CHECK (((remaining_hd >= 0) AND (remaining_hd <= 1000)))
);

ALTER TABLE plan.ticket_estimate ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME plan.ticket_estimate_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE plan.ticket_wsjf (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    ticket_seq bigint NOT NULL,
    business_value integer NOT NULL,
    time_criticality integer NOT NULL,
    risk_opportunity integer NOT NULL,
    valid_from timestamp with time zone DEFAULT now() NOT NULL,
    valid_to timestamp with time zone,
    actor text DEFAULT ''::text NOT NULL,
    actor_kind text DEFAULT ''::text NOT NULL,
    closed_by text,
    CONSTRAINT ticket_wsjf_business_value_check CHECK ((business_value = ANY (ARRAY[1, 2, 3, 5, 8, 13, 21]))),
    CONSTRAINT ticket_wsjf_check CHECK (((valid_to IS NULL) = (closed_by IS NULL))),
    CONSTRAINT ticket_wsjf_risk_opportunity_check CHECK ((risk_opportunity = ANY (ARRAY[1, 2, 3, 5, 8, 13, 21]))),
    CONSTRAINT ticket_wsjf_time_criticality_check CHECK ((time_criticality = ANY (ARRAY[1, 2, 3, 5, 8, 13, 21])))
);

ALTER TABLE plan.ticket_wsjf ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME plan.ticket_wsjf_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE plan.type_default_estimate (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    tag text NOT NULL,
    effort_sp integer NOT NULL,
    valid_from timestamp with time zone DEFAULT now() NOT NULL,
    valid_to timestamp with time zone,
    actor text DEFAULT ''::text NOT NULL,
    actor_kind text DEFAULT ''::text NOT NULL,
    closed_by text,
    CONSTRAINT type_default_estimate_check CHECK (((valid_to IS NULL) = (closed_by IS NULL))),
    CONSTRAINT type_default_estimate_effort_sp_check CHECK ((effort_sp = ANY (ARRAY[1, 2, 3, 5, 8, 13, 21]))),
    CONSTRAINT type_default_estimate_tag_check CHECK (tag = '*' OR (length(tag) BETWEEN 1 AND 60 AND tag !~ '[[:cntrl:]]'))
);

ALTER TABLE plan.type_default_estimate ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME plan.type_default_estimate_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE public.document_ref (
    repo text NOT NULL,
    path text NOT NULL,
    target_repo text NOT NULL,
    target_path text NOT NULL,
    valid_from timestamp with time zone NOT NULL,
    valid_to timestamp with time zone,
    CONSTRAINT document_ref_check CHECK (((valid_to IS NULL) OR (valid_to >= valid_from))),
    CONSTRAINT document_ref_check1 CHECK (((repo <> target_repo) OR (path <> target_path)))
);

CREATE TABLE public.document_repository (
    repo text NOT NULL,
    project text NOT NULL,
    caller text NOT NULL,
    CONSTRAINT document_repository_caller_check CHECK ((caller ~ '^docs-[a-z0-9][a-z0-9-]{0,34}$'::text)),
    CONSTRAINT document_repository_project_check CHECK ((project ~ '^[a-z0-9][a-z0-9-]{0,39}$'::text)),
    CONSTRAINT document_repository_repo_check CHECK ((repo ~ '^[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9._-]{1,100}$'::text))
);

CREATE TABLE public.documents (
    repo text NOT NULL,
    path text NOT NULL,
    role text NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    sha256 text NOT NULL,
    source_revision text,
    created_at timestamp with time zone NOT NULL,
    synced_at timestamp with time zone NOT NULL,
    retired_at timestamp with time zone,
    derived_sha256 text
);

CREATE TABLE public.instance (
    id boolean DEFAULT true NOT NULL,
    account text NOT NULL,
    timezone text NOT NULL,
    mailbox_pending_limit integer,
    mailbox_pending_ttl interval,
    document_roots text[],
    document_link_labels jsonb DEFAULT '{"and": ["and"], "closes": ["Closes"], "describes": ["Ticket", "Tickets"]}'::jsonb NOT NULL,
    mailbox_legacy_import text,
    CONSTRAINT instance_account_check CHECK ((account ~ '^[a-z][a-z0-9-]{0,39}$'::text)),
    CONSTRAINT instance_document_link_labels_check CHECK (public.doc_link_labels_valid(document_link_labels)),
    CONSTRAINT instance_id_check CHECK (id),
    CONSTRAINT instance_mailbox_legacy_import_check CHECK ((mailbox_legacy_import <> ''::text)),
    CONSTRAINT instance_mailbox_pending_limit_check CHECK ((mailbox_pending_limit > 0)),
    CONSTRAINT instance_mailbox_pending_ttl_check CHECK ((mailbox_pending_ttl > '00:00:00'::interval))
);

CREATE TABLE public.legacy_imports (
    source text NOT NULL,
    manifest_sha256 text NOT NULL,
    imported_at timestamp with time zone DEFAULT now() NOT NULL,
    tickets integer NOT NULL,
    kept_hub_edits integer NOT NULL,
    documents integer NOT NULL,
    links integer NOT NULL
);

CREATE TABLE public.repository_document_sync_state (
    repo text NOT NULL,
    revision text NOT NULL,
    synced_at timestamp with time zone NOT NULL,
    problems jsonb DEFAULT '[]'::jsonb NOT NULL,
    CONSTRAINT repository_document_sync_state_problems_check CHECK ((jsonb_typeof(problems) = 'array'::text)),
    CONSTRAINT repository_document_sync_state_revision_check CHECK ((revision ~ '^[a-f0-9]{40}$'::text))
);

CREATE TABLE public.ticket_change (
    id bigint NOT NULL,
    account text DEFAULT public.instance_account() NOT NULL,
    ticket_seq bigint NOT NULL,
    field text NOT NULL,
    old jsonb,
    new jsonb,
    edge_id bigint,
    actor text NOT NULL,
    actor_kind text NOT NULL,
    at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ticket_change_actor_check CHECK (((length(actor) >= 1) AND (length(actor) <= 200))),
    CONSTRAINT ticket_change_actor_kind_check CHECK ((actor_kind ~ '^(human|agent|seed|legacy-pod|service:[a-z][a-z0-9-]{0,39})$'::text)),
    CONSTRAINT ticket_change_field_check CHECK ((field ~ '^[a-z][a-z_:]{1,60}$'::text))
);

ALTER TABLE public.ticket_change ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME public.ticket_change_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

ALTER TABLE public.tickets ALTER COLUMN seq ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME public.tickets_seq_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

ALTER TABLE ONLY auth.account
    ADD CONSTRAINT account_pkey PRIMARY KEY (id);

ALTER TABLE ONLY auth.jwks
    ADD CONSTRAINT jwks_pkey PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthAccessToken"
    ADD CONSTRAINT "oauthAccessToken_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthAccessToken"
    ADD CONSTRAINT "oauthAccessToken_token_key" UNIQUE (token);

ALTER TABLE ONLY auth."oauthClientAssertion"
    ADD CONSTRAINT "oauthClientAssertion_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthClientResource"
    ADD CONSTRAINT "oauthClientResource_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthClient"
    ADD CONSTRAINT "oauthClient_clientId_key" UNIQUE ("clientId");

ALTER TABLE ONLY auth."oauthClient"
    ADD CONSTRAINT "oauthClient_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthConsent"
    ADD CONSTRAINT "oauthConsent_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthRefreshToken"
    ADD CONSTRAINT "oauthRefreshToken_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth."oauthRefreshToken"
    ADD CONSTRAINT "oauthRefreshToken_token_key" UNIQUE (token);

ALTER TABLE ONLY auth."oauthResource"
    ADD CONSTRAINT "oauthResource_identifier_key" UNIQUE (identifier);

ALTER TABLE ONLY auth."oauthResource"
    ADD CONSTRAINT "oauthResource_pkey" PRIMARY KEY (id);

ALTER TABLE ONLY auth.session
    ADD CONSTRAINT session_pkey PRIMARY KEY (id);

ALTER TABLE ONLY auth.session
    ADD CONSTRAINT session_token_key UNIQUE (token);

ALTER TABLE ONLY auth."user"
    ADD CONSTRAINT user_email_key UNIQUE (email);

ALTER TABLE ONLY auth."user"
    ADD CONSTRAINT user_pkey PRIMARY KEY (id);

ALTER TABLE ONLY auth.verification
    ADD CONSTRAINT verification_pkey PRIMARY KEY (id);

ALTER TABLE ONLY graph.component
    ADD CONSTRAINT component_account_slug_key UNIQUE (account, slug);

ALTER TABLE ONLY graph.component
    ADD CONSTRAINT component_pkey PRIMARY KEY (node_id);

ALTER TABLE ONLY graph.document
    ADD CONSTRAINT document_account_repo_path_key UNIQUE (account, repo, path);

ALTER TABLE ONLY graph.document
    ADD CONSTRAINT document_pkey PRIMARY KEY (node_id);

ALTER TABLE ONLY graph.edge
    ADD CONSTRAINT edge_pkey PRIMARY KEY (id);

ALTER TABLE ONLY graph.node
    ADD CONSTRAINT node_account_id_kind_key UNIQUE (account, id, kind);

ALTER TABLE ONLY graph.node
    ADD CONSTRAINT node_account_kind_key_key UNIQUE (account, kind, key);

ALTER TABLE ONLY graph.node_kind
    ADD CONSTRAINT node_kind_pkey PRIMARY KEY (account, kind);

ALTER TABLE ONLY graph.node
    ADD CONSTRAINT node_pkey PRIMARY KEY (id);

ALTER TABLE ONLY graph.person
    ADD CONSTRAINT person_account_handle_key UNIQUE (account, handle);

ALTER TABLE ONLY graph.person_login
    ADD CONSTRAINT person_login_pkey PRIMARY KEY (id);

ALTER TABLE ONLY graph.person
    ADD CONSTRAINT person_pkey PRIMARY KEY (node_id);

ALTER TABLE ONLY graph.rel_shape
    ADD CONSTRAINT rel_shape_pkey PRIMARY KEY (account, rel, src_kind, dst_kind);

ALTER TABLE ONLY graph.ticket
    ADD CONSTRAINT ticket_account_ticket_seq_key UNIQUE (account, ticket_seq);

ALTER TABLE ONLY graph.ticket
    ADD CONSTRAINT ticket_pkey PRIMARY KEY (node_id);

ALTER TABLE ONLY mailbox.cursors
    ADD CONSTRAINT cursors_pkey PRIMARY KEY (key_id, agent);

ALTER TABLE ONLY mailbox.imports
    ADD CONSTRAINT imports_pkey PRIMARY KEY (source);

ALTER TABLE ONLY mailbox.key_review
    ADD CONSTRAINT key_review_pkey PRIMARY KEY (id);

ALTER TABLE ONLY mailbox.keys
    ADD CONSTRAINT keys_pkey PRIMARY KEY (id);

ALTER TABLE ONLY mailbox.messages
    ADD CONSTRAINT messages_pkey PRIMARY KEY (id);

ALTER TABLE ONLY plan.baseline
    ADD CONSTRAINT baseline_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY plan.baseline
    ADD CONSTRAINT baseline_pkey PRIMARY KEY (id);

ALTER TABLE ONLY plan.current
    ADD CONSTRAINT current_pkey PRIMARY KEY (account, root_seq);

ALTER TABLE ONLY plan.dirty
    ADD CONSTRAINT dirty_pkey PRIMARY KEY (account, root_seq);

ALTER TABLE ONLY plan.fever_point
    ADD CONSTRAINT fever_point_pkey PRIMARY KEY (account, root_seq, day, buffer_key);

ALTER TABLE ONLY plan.points_to_hd
    ADD CONSTRAINT points_to_hd_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY plan.points_to_hd
    ADD CONSTRAINT points_to_hd_pkey PRIMARY KEY (id);

ALTER TABLE ONLY plan.scope
    ADD CONSTRAINT scope_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY plan.scope
    ADD CONSTRAINT scope_pkey PRIMARY KEY (id);

ALTER TABLE ONLY plan.ticket_estimate
    ADD CONSTRAINT ticket_estimate_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY plan.ticket_estimate
    ADD CONSTRAINT ticket_estimate_pkey PRIMARY KEY (id);

ALTER TABLE ONLY plan.ticket_wsjf
    ADD CONSTRAINT ticket_wsjf_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY plan.ticket_wsjf
    ADD CONSTRAINT ticket_wsjf_pkey PRIMARY KEY (id);

ALTER TABLE ONLY plan.type_default_estimate
    ADD CONSTRAINT type_default_estimate_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY plan.type_default_estimate
    ADD CONSTRAINT type_default_estimate_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.document_ref
    ADD CONSTRAINT document_ref_pkey PRIMARY KEY (repo, path, target_repo, target_path, valid_from);

ALTER TABLE ONLY public.document_repository
    ADD CONSTRAINT document_repository_caller_key UNIQUE (caller);

ALTER TABLE ONLY public.document_repository
    ADD CONSTRAINT document_repository_pkey PRIMARY KEY (repo);

ALTER TABLE ONLY public.documents
    ADD CONSTRAINT documents_pkey PRIMARY KEY (repo, path);

ALTER TABLE ONLY public.instance
    ADD CONSTRAINT instance_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.legacy_imports
    ADD CONSTRAINT legacy_imports_pkey PRIMARY KEY (source, manifest_sha256);

ALTER TABLE ONLY public.repository_document_sync_state
    ADD CONSTRAINT repository_document_sync_state_pkey PRIMARY KEY (repo);

ALTER TABLE ONLY public.ticket_change
    ADD CONSTRAINT ticket_change_account_id_key UNIQUE (account, id);

ALTER TABLE ONLY public.ticket_change
    ADD CONSTRAINT ticket_change_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.ticket_documents
    ADD CONSTRAINT ticket_documents_pkey PRIMARY KEY (ticket_seq, repo, path, rel, valid_from);

ALTER TABLE ONLY public.tickets
    ADD CONSTRAINT tickets_code_key UNIQUE (code);

ALTER TABLE ONLY public.tickets
    ADD CONSTRAINT tickets_legacy_id_key UNIQUE (legacy_id);

ALTER TABLE ONLY public.tickets
    ADD CONSTRAINT tickets_pkey PRIMARY KEY (seq);

CREATE INDEX "account_userId_idx" ON auth.account USING btree ("userId");

CREATE INDEX "oauthAccessToken_authorizationCodeId_idx" ON auth."oauthAccessToken" USING btree ("authorizationCodeId");

CREATE INDEX "oauthAccessToken_clientId_idx" ON auth."oauthAccessToken" USING btree ("clientId");

CREATE INDEX "oauthAccessToken_refreshId_idx" ON auth."oauthAccessToken" USING btree ("refreshId");

CREATE INDEX "oauthAccessToken_sessionId_idx" ON auth."oauthAccessToken" USING btree ("sessionId");

CREATE INDEX "oauthAccessToken_userId_idx" ON auth."oauthAccessToken" USING btree ("userId");

CREATE INDEX "oauthClientResource_clientId_idx" ON auth."oauthClientResource" USING btree ("clientId");

CREATE UNIQUE INDEX "oauthClientResource_clientId_resourceId_uidx" ON auth."oauthClientResource" USING btree ("clientId", "resourceId");

CREATE INDEX "oauthClientResource_resourceId_idx" ON auth."oauthClientResource" USING btree ("resourceId");

CREATE INDEX "oauthClient_userId_idx" ON auth."oauthClient" USING btree ("userId");

CREATE INDEX "oauthConsent_clientId_idx" ON auth."oauthConsent" USING btree ("clientId");

CREATE INDEX "oauthConsent_userId_idx" ON auth."oauthConsent" USING btree ("userId");

CREATE INDEX "oauthRefreshToken_authorizationCodeId_idx" ON auth."oauthRefreshToken" USING btree ("authorizationCodeId");

CREATE INDEX "oauthRefreshToken_clientId_idx" ON auth."oauthRefreshToken" USING btree ("clientId");

CREATE INDEX "oauthRefreshToken_sessionId_idx" ON auth."oauthRefreshToken" USING btree ("sessionId");

CREATE INDEX "oauthRefreshToken_userId_idx" ON auth."oauthRefreshToken" USING btree ("userId");

CREATE INDEX "session_userId_idx" ON auth.session USING btree ("userId");

CREATE INDEX verification_identifier_idx ON auth.verification USING btree (identifier);

CREATE INDEX edge_dst_live ON graph.edge USING btree (dst, rel) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX edge_live_unique ON graph.edge USING btree (account, src, rel, dst) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX edge_single ON graph.edge USING btree (account, src, rel) WHERE ((valid_to IS NULL) AND (state = 'confirmed'::text) AND (rel = ANY (ARRAY['assigned_to'::text, 'part_of'::text, 'duplicates'::text, 'component_of'::text])));

CREATE INDEX edge_src_live ON graph.edge USING btree (src, rel) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX person_email_unique ON graph.person USING btree (account, lower(email));

CREATE UNIQUE INDEX person_login_handle ON graph.person_login USING btree (account, handle) WHERE (retired_at IS NULL);

CREATE UNIQUE INDEX person_login_user ON graph.person_login USING btree (account, user_id) WHERE (retired_at IS NULL);

CREATE INDEX key_review_key ON mailbox.key_review USING btree (key_id, id);

CREATE INDEX messages_key_id ON mailbox.messages USING btree (key_id, id);

CREATE UNIQUE INDEX baseline_succession ON plan.baseline USING btree (account, root_seq, supersedes) NULLS NOT DISTINCT;

CREATE INDEX dirty_pending ON plan.dirty USING btree (account, since) WHERE (marked > done);

CREATE INDEX estimate_history ON plan.ticket_estimate USING btree (account, ticket_seq, id);

CREATE UNIQUE INDEX estimate_live ON plan.ticket_estimate USING btree (account, ticket_seq) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX points_live ON plan.points_to_hd USING btree (account, effort_sp) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX scope_live ON plan.scope USING btree (account, root_seq) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX type_default_live ON plan.type_default_estimate USING btree (account, tag) WHERE (valid_to IS NULL);

CREATE INDEX wsjf_history ON plan.ticket_wsjf USING btree (account, ticket_seq, id);

CREATE UNIQUE INDEX wsjf_live ON plan.ticket_wsjf USING btree (account, ticket_seq) WHERE (valid_to IS NULL);

CREATE UNIQUE INDEX document_ref_live ON public.document_ref USING btree (repo, path, target_repo, target_path) WHERE (valid_to IS NULL);

CREATE INDEX document_ref_target ON public.document_ref USING btree (target_path, target_repo) WHERE (valid_to IS NULL);

CREATE INDEX documents_topic ON public.documents USING btree (repo, public.doc_topic(path));

CREATE INDEX ticket_change_ticket ON public.ticket_change USING btree (account, ticket_seq, id);

CREATE INDEX ticket_documents_doc ON public.ticket_documents USING btree (repo, path);

CREATE INDEX tickets_project_seq ON public.tickets USING btree (project, seq);

CREATE INDEX tickets_status_project_seq ON public.tickets USING btree (status, project, seq DESC);

CREATE INDEX tickets_status_seq ON public.tickets USING btree (status, seq);

CREATE INDEX tickets_tags ON public.tickets USING gin (tags);

CREATE INDEX tickets_updated ON public.tickets USING btree (updated_at DESC);

-- Reference data under the template account: node kinds, edge shapes and default story points. The installation
-- row (public.seed_instance, called by the migration job after the migrations) copies them to the installation
-- account. Inserted before the triggers: the plan triggers read the installation account, which a fresh database
-- does not have yet, so the version columns of the default points are written here as plan.version_guard would.

INSERT INTO graph.node_kind (account, kind) VALUES
    ('template', 'ticket'),
    ('template', 'project'),
    ('template', 'component'),
    ('template', 'person'),
    ('template', 'meeting'),
    ('template', 'document');

INSERT INTO graph.rel_shape (account, rel, src_kind, dst_kind, single, acyclic, needs_confirm) VALUES
    ('template', 'blocks', 'ticket', 'ticket', false, true, false),
    ('template', 'relates_to', 'ticket', 'ticket', false, false, false),
    ('template', 'duplicates', 'ticket', 'ticket', true, true, false),
    ('template', 'part_of', 'ticket', 'ticket', true, true, false),
    ('template', 'in_component', 'ticket', 'component', false, false, false),
    ('template', 'assigned_to', 'ticket', 'person', true, false, false),
    ('template', 'component_of', 'component', 'project', true, false, false),
    ('template', 'mentioned_in', 'ticket', 'meeting', false, false, true),
    ('template', 'decided_in', 'ticket', 'meeting', false, false, true),
    ('template', 'raised_in', 'ticket', 'meeting', false, false, true),
    ('template', 'attended', 'person', 'meeting', false, false, false),
    ('template', 'about', 'meeting', 'project', false, false, false),
    ('template', 'implements', 'document', 'document', false, false, false),
    ('template', 'specifies', 'document', 'document', false, false, false),
    ('template', 'references', 'document', 'document', false, false, false),
    ('template', 'in_project', 'document', 'project', false, false, false);

INSERT INTO plan.type_default_estimate (account, tag, effort_sp, actor, actor_kind) VALUES
    ('template', 'bug', 2, 'seed:baseline', 'seed'),
    ('template', 'chore', 2, 'seed:baseline', 'seed'),
    ('template', 'feature', 5, 'seed:baseline', 'seed'),
    ('template', '*', 3, 'seed:baseline', 'seed');

CREATE TRIGGER attr_guard BEFORE INSERT OR UPDATE ON graph.component FOR EACH ROW EXECUTE FUNCTION graph.attr_guard();

CREATE TRIGGER attr_guard BEFORE INSERT OR UPDATE ON graph.person FOR EACH ROW EXECUTE FUNCTION graph.attr_guard();

CREATE TRIGGER edge_a_guard BEFORE UPDATE ON graph.edge FOR EACH ROW EXECUTE FUNCTION graph.edge_guard();

CREATE TRIGGER edge_b_check BEFORE INSERT OR UPDATE ON graph.edge FOR EACH ROW EXECUTE FUNCTION graph.edge_check();

CREATE TRIGGER edge_c_supersede BEFORE INSERT OR UPDATE OF state ON graph.edge FOR EACH ROW WHEN ((new.rel = 'assigned_to'::text)) EXECUTE FUNCTION graph.edge_supersede();

CREATE TRIGGER edge_change AFTER INSERT OR UPDATE ON graph.edge FOR EACH ROW EXECUTE FUNCTION public.edge_change_log();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.component FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.document FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.edge FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.node FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.node_kind FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.person FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.person_login FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.rel_shape FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON graph.ticket FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.component FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.document FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.edge FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.node FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.node_kind FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.person FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.person_login FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.rel_shape FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON graph.ticket FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_update BEFORE UPDATE ON graph.document FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER forbid_update BEFORE UPDATE ON graph.node_kind FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER forbid_update BEFORE UPDATE ON graph.rel_shape FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER forbid_update BEFORE UPDATE ON graph.ticket FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER node_guard BEFORE UPDATE ON graph.node FOR EACH ROW EXECUTE FUNCTION graph.node_guard();

CREATE TRIGGER person_login_guard BEFORE INSERT OR UPDATE ON graph.person_login FOR EACH ROW EXECUTE FUNCTION graph.person_login_guard();

CREATE TRIGGER person_login_key_owner AFTER INSERT ON graph.person_login FOR EACH ROW EXECUTE FUNCTION graph.person_login_key_owner();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON graph.edge FOR EACH ROW WHEN ((new.rel = ANY (ARRAY['blocks'::text, 'part_of'::text, 'duplicates'::text, 'assigned_to'::text]))) EXECUTE FUNCTION plan.edge_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON graph.edge DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN ((new.rel = ANY (ARRAY['blocks'::text, 'part_of'::text, 'duplicates'::text, 'assigned_to'::text]))) EXECUTE FUNCTION plan.flush_dirty();

CREATE TRIGGER forbid_delete BEFORE DELETE ON mailbox.key_review FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON mailbox.key_review FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_update BEFORE UPDATE ON mailbox.key_review FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER key_status_guard BEFORE UPDATE ON mailbox.keys FOR EACH ROW WHEN ((old.status IS DISTINCT FROM new.status)) EXECUTE FUNCTION mailbox.key_status_guard();

CREATE TRIGGER baseline_guard BEFORE INSERT ON plan.baseline FOR EACH ROW EXECUTE FUNCTION plan.baseline_guard();

CREATE TRIGGER dirty_guard BEFORE INSERT OR UPDATE ON plan.dirty FOR EACH ROW EXECUTE FUNCTION plan.dirty_guard();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.baseline FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.current FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.dirty FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.fever_point FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.points_to_hd FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.scope FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.ticket_estimate FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.ticket_wsjf FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON plan.type_default_estimate FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.baseline FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.current FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.dirty FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.fever_point FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.points_to_hd FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.scope FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.ticket_estimate FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.ticket_wsjf FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON plan.type_default_estimate FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_update BEFORE UPDATE ON plan.baseline FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER plan_dirty AFTER INSERT ON plan.baseline FOR EACH ROW EXECUTE FUNCTION plan.root_dirty();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON plan.current FOR EACH ROW EXECUTE FUNCTION plan.current_dirty();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON plan.points_to_hd FOR EACH STATEMENT EXECUTE FUNCTION plan.all_dirty();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON plan.scope FOR EACH ROW EXECUTE FUNCTION plan.root_dirty();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON plan.ticket_estimate FOR EACH ROW EXECUTE FUNCTION plan.estimate_dirty();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON plan.ticket_wsjf FOR EACH ROW EXECUTE FUNCTION plan.estimate_dirty();

CREATE TRIGGER plan_dirty AFTER INSERT OR UPDATE ON plan.type_default_estimate FOR EACH STATEMENT EXECUTE FUNCTION plan.all_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT ON plan.baseline DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON plan.current DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON plan.points_to_hd DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON plan.scope DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON plan.ticket_estimate DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON plan.ticket_wsjf DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER INSERT OR UPDATE ON plan.type_default_estimate DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION plan.flush_dirty();

CREATE TRIGGER scope_guard BEFORE INSERT ON plan.scope FOR EACH ROW EXECUTE FUNCTION plan.scope_guard();

CREATE TRIGGER service_guard BEFORE INSERT OR UPDATE ON plan.current FOR EACH ROW EXECUTE FUNCTION plan.service_guard();

CREATE TRIGGER service_guard BEFORE INSERT OR UPDATE ON plan.fever_point FOR EACH ROW EXECUTE FUNCTION plan.service_guard();

CREATE TRIGGER version_guard BEFORE INSERT OR UPDATE ON plan.points_to_hd FOR EACH ROW EXECUTE FUNCTION plan.version_guard('effort_sp', 'human,seed');

CREATE TRIGGER version_guard BEFORE INSERT OR UPDATE ON plan.scope FOR EACH ROW EXECUTE FUNCTION plan.version_guard('root_seq', 'human,agent,seed');

CREATE TRIGGER version_guard BEFORE INSERT OR UPDATE ON plan.ticket_estimate FOR EACH ROW EXECUTE FUNCTION plan.version_guard('ticket_seq', 'human,agent,seed');

CREATE TRIGGER version_guard BEFORE INSERT OR UPDATE ON plan.ticket_wsjf FOR EACH ROW EXECUTE FUNCTION plan.version_guard('ticket_seq', 'human,agent,seed');

CREATE TRIGGER version_guard BEFORE INSERT OR UPDATE ON plan.type_default_estimate FOR EACH ROW EXECUTE FUNCTION plan.version_guard('tag', 'human,seed');

CREATE TRIGGER account_fixed BEFORE UPDATE ON public.instance FOR EACH ROW EXECUTE FUNCTION public.instance_account_fixed();

CREATE TRIGGER document_node AFTER INSERT ON public.documents FOR EACH ROW EXECUTE FUNCTION graph.document_node();

CREATE TRIGGER forbid_delete BEFORE DELETE ON public.document_ref FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON public.document_repository FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON public.documents FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON public.ticket_change FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON public.ticket_documents FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_delete BEFORE DELETE ON public.tickets FOR EACH ROW EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON public.document_ref FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON public.document_repository FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON public.documents FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON public.ticket_change FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON public.ticket_documents FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_truncate BEFORE TRUNCATE ON public.tickets FOR EACH STATEMENT EXECUTE FUNCTION graph.forbid_delete();

CREATE TRIGGER forbid_update BEFORE UPDATE ON public.ticket_change FOR EACH ROW EXECUTE FUNCTION graph.forbid_update();

CREATE TRIGGER plan_dirty AFTER UPDATE ON public.tickets FOR EACH ROW WHEN ((OLD.status, OLD.priority, OLD.tags, OLD.code) IS DISTINCT FROM (NEW.status, NEW.priority, NEW.tags, NEW.code)) EXECUTE FUNCTION plan.ticket_dirty();

CREATE CONSTRAINT TRIGGER plan_flush AFTER UPDATE ON public.tickets DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN ((OLD.status, OLD.priority, OLD.tags, OLD.code) IS DISTINCT FROM (NEW.status, NEW.priority, NEW.tags, NEW.code)) EXECUTE FUNCTION plan.flush_dirty();

CREATE TRIGGER project_node AFTER INSERT OR UPDATE OF project ON public.tickets FOR EACH ROW EXECUTE FUNCTION graph.project_node();

CREATE TRIGGER ticket_change AFTER UPDATE ON public.tickets FOR EACH ROW EXECUTE FUNCTION public.ticket_change_log();

CREATE TRIGGER ticket_created AFTER INSERT ON public.tickets FOR EACH ROW EXECUTE FUNCTION public.ticket_create_log();

CREATE TRIGGER ticket_node AFTER INSERT ON public.tickets FOR EACH ROW EXECUTE FUNCTION graph.ticket_node();

ALTER TABLE ONLY auth.account
    ADD CONSTRAINT "account_userId_fkey" FOREIGN KEY ("userId") REFERENCES auth."user"(id) ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthAccessToken"
    ADD CONSTRAINT "oauthAccessToken_clientId_fkey" FOREIGN KEY ("clientId") REFERENCES auth."oauthClient"("clientId") ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthAccessToken"
    ADD CONSTRAINT "oauthAccessToken_refreshId_fkey" FOREIGN KEY ("refreshId") REFERENCES auth."oauthRefreshToken"(id) ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthAccessToken"
    ADD CONSTRAINT "oauthAccessToken_sessionId_fkey" FOREIGN KEY ("sessionId") REFERENCES auth.session(id) ON DELETE SET NULL;

ALTER TABLE ONLY auth."oauthAccessToken"
    ADD CONSTRAINT "oauthAccessToken_userId_fkey" FOREIGN KEY ("userId") REFERENCES auth."user"(id) ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthClientResource"
    ADD CONSTRAINT "oauthClientResource_clientId_fkey" FOREIGN KEY ("clientId") REFERENCES auth."oauthClient"("clientId") ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthClientResource"
    ADD CONSTRAINT "oauthClientResource_resourceId_fkey" FOREIGN KEY ("resourceId") REFERENCES auth."oauthResource"(identifier) ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthClient"
    ADD CONSTRAINT "oauthClient_userId_fkey" FOREIGN KEY ("userId") REFERENCES auth."user"(id) ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthConsent"
    ADD CONSTRAINT "oauthConsent_clientId_fkey" FOREIGN KEY ("clientId") REFERENCES auth."oauthClient"("clientId") ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthConsent"
    ADD CONSTRAINT "oauthConsent_userId_fkey" FOREIGN KEY ("userId") REFERENCES auth."user"(id) ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthRefreshToken"
    ADD CONSTRAINT "oauthRefreshToken_clientId_fkey" FOREIGN KEY ("clientId") REFERENCES auth."oauthClient"("clientId") ON DELETE CASCADE;

ALTER TABLE ONLY auth."oauthRefreshToken"
    ADD CONSTRAINT "oauthRefreshToken_sessionId_fkey" FOREIGN KEY ("sessionId") REFERENCES auth.session(id) ON DELETE SET NULL;

ALTER TABLE ONLY auth."oauthRefreshToken"
    ADD CONSTRAINT "oauthRefreshToken_userId_fkey" FOREIGN KEY ("userId") REFERENCES auth."user"(id) ON DELETE CASCADE;

ALTER TABLE ONLY auth.session
    ADD CONSTRAINT "session_userId_fkey" FOREIGN KEY ("userId") REFERENCES auth."user"(id) ON DELETE CASCADE;

ALTER TABLE ONLY graph.component
    ADD CONSTRAINT component_account_node_id_kind_fkey FOREIGN KEY (account, node_id, kind) REFERENCES graph.node(account, id, kind);

ALTER TABLE ONLY graph.document
    ADD CONSTRAINT document_account_node_id_kind_fkey FOREIGN KEY (account, node_id, kind) REFERENCES graph.node(account, id, kind);

ALTER TABLE ONLY graph.document
    ADD CONSTRAINT document_repo_path_fkey FOREIGN KEY (repo, path) REFERENCES public.documents(repo, path);

ALTER TABLE ONLY graph.edge
    ADD CONSTRAINT edge_account_dst_dst_kind_fkey FOREIGN KEY (account, dst, dst_kind) REFERENCES graph.node(account, id, kind);

ALTER TABLE ONLY graph.edge
    ADD CONSTRAINT edge_account_rel_src_kind_dst_kind_fkey FOREIGN KEY (account, rel, src_kind, dst_kind) REFERENCES graph.rel_shape(account, rel, src_kind, dst_kind);

ALTER TABLE ONLY graph.edge
    ADD CONSTRAINT edge_account_src_src_kind_fkey FOREIGN KEY (account, src, src_kind) REFERENCES graph.node(account, id, kind);

ALTER TABLE ONLY graph.node
    ADD CONSTRAINT node_account_kind_fkey FOREIGN KEY (account, kind) REFERENCES graph.node_kind(account, kind);

ALTER TABLE ONLY graph.person
    ADD CONSTRAINT person_account_node_id_kind_fkey FOREIGN KEY (account, node_id, kind) REFERENCES graph.node(account, id, kind);

ALTER TABLE ONLY graph.person_login
    ADD CONSTRAINT person_login_account_handle_fkey FOREIGN KEY (account, handle) REFERENCES graph.person(account, handle);

ALTER TABLE ONLY graph.rel_shape
    ADD CONSTRAINT rel_shape_account_dst_kind_fkey FOREIGN KEY (account, dst_kind) REFERENCES graph.node_kind(account, kind);

ALTER TABLE ONLY graph.rel_shape
    ADD CONSTRAINT rel_shape_account_src_kind_fkey FOREIGN KEY (account, src_kind) REFERENCES graph.node_kind(account, kind);

ALTER TABLE ONLY graph.ticket
    ADD CONSTRAINT ticket_account_node_id_kind_fkey FOREIGN KEY (account, node_id, kind) REFERENCES graph.node(account, id, kind);

ALTER TABLE ONLY graph.ticket
    ADD CONSTRAINT ticket_ticket_seq_fkey FOREIGN KEY (ticket_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY mailbox.cursors
    ADD CONSTRAINT cursors_key_id_fkey FOREIGN KEY (key_id) REFERENCES mailbox.keys(id);

ALTER TABLE ONLY mailbox.key_review
    ADD CONSTRAINT key_review_key_id_fkey FOREIGN KEY (key_id) REFERENCES mailbox.keys(id);

ALTER TABLE ONLY mailbox.messages
    ADD CONSTRAINT messages_key_id_fkey FOREIGN KEY (key_id) REFERENCES mailbox.keys(id);

ALTER TABLE ONLY plan.baseline
    ADD CONSTRAINT baseline_account_scope_id_fkey FOREIGN KEY (account, scope_id) REFERENCES plan.scope(account, id);

ALTER TABLE ONLY plan.baseline
    ADD CONSTRAINT baseline_account_supersedes_fkey FOREIGN KEY (account, supersedes) REFERENCES plan.baseline(account, id);

ALTER TABLE ONLY plan.baseline
    ADD CONSTRAINT baseline_root_seq_fkey FOREIGN KEY (root_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY plan.current
    ADD CONSTRAINT current_account_baseline_id_fkey FOREIGN KEY (account, baseline_id) REFERENCES plan.baseline(account, id);

ALTER TABLE ONLY plan.current
    ADD CONSTRAINT current_root_seq_fkey FOREIGN KEY (root_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY plan.dirty
    ADD CONSTRAINT dirty_root_seq_fkey FOREIGN KEY (root_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY plan.fever_point
    ADD CONSTRAINT fever_point_account_baseline_id_fkey FOREIGN KEY (account, baseline_id) REFERENCES plan.baseline(account, id);

ALTER TABLE ONLY plan.fever_point
    ADD CONSTRAINT fever_point_root_seq_fkey FOREIGN KEY (root_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY plan.scope
    ADD CONSTRAINT scope_root_seq_fkey FOREIGN KEY (root_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY plan.ticket_estimate
    ADD CONSTRAINT ticket_estimate_ticket_seq_fkey FOREIGN KEY (ticket_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY plan.ticket_wsjf
    ADD CONSTRAINT ticket_wsjf_ticket_seq_fkey FOREIGN KEY (ticket_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY public.document_ref
    ADD CONSTRAINT document_ref_repo_path_fkey FOREIGN KEY (repo, path) REFERENCES public.documents(repo, path);

ALTER TABLE ONLY public.repository_document_sync_state
    ADD CONSTRAINT repository_document_sync_state_repo_fkey FOREIGN KEY (repo) REFERENCES public.document_repository(repo);

ALTER TABLE ONLY public.ticket_change
    ADD CONSTRAINT ticket_change_edge_id_fkey FOREIGN KEY (edge_id) REFERENCES graph.edge(id);

ALTER TABLE ONLY public.ticket_change
    ADD CONSTRAINT ticket_change_ticket_seq_fkey FOREIGN KEY (ticket_seq) REFERENCES public.tickets(seq);

ALTER TABLE ONLY public.ticket_documents
    ADD CONSTRAINT ticket_documents_repo_path_fkey FOREIGN KEY (repo, path) REFERENCES public.documents(repo, path) ON DELETE CASCADE;

ALTER TABLE ONLY public.ticket_documents
    ADD CONSTRAINT ticket_documents_ticket_seq_fkey FOREIGN KEY (ticket_seq) REFERENCES public.tickets(seq) ON DELETE CASCADE;

GRANT USAGE ON SCHEMA auth TO hub_web;

GRANT USAGE ON SCHEMA graph TO hub;

GRANT USAGE ON SCHEMA mailbox TO hub;

GRANT USAGE ON SCHEMA plan TO hub;

GRANT USAGE ON SCHEMA public TO hub;

REVOKE ALL ON FUNCTION graph.attr_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.attr_guard() TO hub;

REVOKE ALL ON FUNCTION graph.document_node() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.document_node() TO hub;

REVOKE ALL ON FUNCTION graph.edge_check() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.edge_check() TO hub;

REVOKE ALL ON FUNCTION graph.edge_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.edge_guard() TO hub;

REVOKE ALL ON FUNCTION graph.edge_supersede() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.edge_supersede() TO hub;

REVOKE ALL ON FUNCTION graph.forbid_delete() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.forbid_delete() TO hub;

REVOKE ALL ON FUNCTION graph.forbid_update() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.forbid_update() TO hub;

REVOKE ALL ON FUNCTION graph.import_components(p_components jsonb) FROM PUBLIC;
GRANT ALL ON FUNCTION graph.import_components(p_components jsonb) TO hub;

REVOKE ALL ON FUNCTION graph.import_links(p_links jsonb, p_seed text) FROM PUBLIC;
GRANT ALL ON FUNCTION graph.import_links(p_links jsonb, p_seed text) TO hub;

REVOKE ALL ON FUNCTION graph.import_people(p_people jsonb) FROM PUBLIC;
GRANT ALL ON FUNCTION graph.import_people(p_people jsonb) TO hub;

REVOKE ALL ON FUNCTION graph.node_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.node_guard() TO hub;

GRANT ALL ON FUNCTION graph.node_label(p_node bigint) TO hub;

REVOKE ALL ON FUNCTION graph.person_login_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.person_login_guard() TO hub;

REVOKE ALL ON FUNCTION graph.person_login_key_owner() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.person_login_key_owner() TO hub;

REVOKE ALL ON FUNCTION graph.project_node() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.project_node() TO hub;

REVOKE ALL ON FUNCTION graph.require_fresh_human() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.require_fresh_human() TO hub;

REVOKE ALL ON FUNCTION graph.ticket_node() FROM PUBLIC;
GRANT ALL ON FUNCTION graph.ticket_node() TO hub;

GRANT ALL ON FUNCTION mailbox.exchange(p_key text, p_owner text, p_agent text, p_msg text, p_now timestamp with time zone, p_trusted boolean) TO hub;

GRANT ALL ON FUNCTION mailbox.import_history(snapshot jsonb) TO hub;

REVOKE ALL ON FUNCTION mailbox.key_status_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION mailbox.key_status_guard() TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE mailbox.keys TO hub;

REVOKE ALL ON FUNCTION mailbox.review_key(p_id text, p_status text) FROM PUBLIC;
GRANT ALL ON FUNCTION mailbox.review_key(p_id text, p_status text) TO hub;

REVOKE ALL ON FUNCTION plan.all_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.all_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.baseline_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.baseline_guard() TO hub;

REVOKE ALL ON FUNCTION plan.current_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.current_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.dirty_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.dirty_guard() TO hub;

REVOKE ALL ON FUNCTION plan.edge_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.edge_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.estimate_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.estimate_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.flush(p_account text, p_root bigint, p_done bigint) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.flush(p_account text, p_root bigint, p_done bigint) TO hub;

REVOKE ALL ON FUNCTION plan.flush_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.flush_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.import_scopes(p_scopes jsonb) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.import_scopes(p_scopes jsonb) TO hub;

REVOKE ALL ON FUNCTION plan.mark(p_account text, p_roots bigint[]) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.mark(p_account text, p_roots bigint[]) TO hub;

REVOKE ALL ON FUNCTION plan.mark_all(p_account text) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.mark_all(p_account text) TO hub;

REVOKE ALL ON FUNCTION plan.mark_all_now(p_account text) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.mark_all_now(p_account text) TO hub;

REVOKE ALL ON FUNCTION plan.mark_now(p_account text, p_roots bigint[]) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.mark_now(p_account text, p_roots bigint[]) TO hub;

REVOKE ALL ON FUNCTION plan.require_actor(p_kinds text[]) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.require_actor(p_kinds text[]) TO hub;

REVOKE ALL ON FUNCTION plan.root_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.root_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.roots_of(p_account text, p_seqs bigint[]) FROM PUBLIC;
GRANT ALL ON FUNCTION plan.roots_of(p_account text, p_seqs bigint[]) TO hub;

REVOKE ALL ON FUNCTION plan.scope_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.scope_guard() TO hub;

REVOKE ALL ON FUNCTION plan.service_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.service_guard() TO hub;

REVOKE ALL ON FUNCTION plan.ticket_dirty() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.ticket_dirty() TO hub;

REVOKE ALL ON FUNCTION plan.version_guard() FROM PUBLIC;
GRANT ALL ON FUNCTION plan.version_guard() TO hub;

REVOKE ALL ON FUNCTION public.doc_apply(p_repo text, p_path text, p_extract jsonb, p_close boolean, p_mark boolean) FROM PUBLIC;
GRANT ALL ON FUNCTION public.doc_apply(p_repo text, p_path text, p_extract jsonb, p_close boolean, p_mark boolean) TO hub;

GRANT ALL ON FUNCTION public.doc_date(p_path text) TO hub;

REVOKE ALL ON FUNCTION public.doc_extract(p_repo text, p_path text, p_body text, p_known text[], p_labels jsonb) FROM PUBLIC;
GRANT ALL ON FUNCTION public.doc_extract(p_repo text, p_path text, p_body text, p_known text[], p_labels jsonb) TO hub;

GRANT ALL ON FUNCTION public.doc_link_label_pattern(p_labels jsonb, p_key text) TO hub;

GRANT ALL ON FUNCTION public.doc_link_labels_valid(p_labels jsonb) TO hub;

GRANT ALL ON FUNCTION public.doc_link_rel(p_src text, p_dst text) TO hub;

GRANT ALL ON FUNCTION public.doc_name(p_path text) TO hub;

GRANT ALL ON FUNCTION public.doc_path_refs(p_text text, p_repo text, p_path text, p_repos text[]) TO hub;

REVOKE ALL ON FUNCTION public.doc_reconcile(p_repo text, p_path text, p_revision text) FROM PUBLIC;
GRANT ALL ON FUNCTION public.doc_reconcile(p_repo text, p_path text, p_revision text) TO hub;

REVOKE ALL ON FUNCTION public.doc_retire(p_repo text, p_path text) FROM PUBLIC;
GRANT ALL ON FUNCTION public.doc_retire(p_repo text, p_path text) TO hub;

GRANT ALL ON FUNCTION public.doc_role(p_path text) TO hub;

GRANT ALL ON FUNCTION public.doc_strip_code(p_body text) TO hub;

GRANT ALL ON FUNCTION public.doc_ticket_tokens(p_text text, p_labels jsonb) TO hub;

GRANT ALL ON FUNCTION public.doc_title(p_path text, p_body text) TO hub;

GRANT ALL ON FUNCTION public.doc_topic(p_path text) TO hub;

GRANT ALL ON FUNCTION public.doc_valid_path(p_path text) TO hub;

REVOKE ALL ON FUNCTION public.edge_change_log() FROM PUBLIC;
GRANT ALL ON FUNCTION public.edge_change_log() TO hub;

REVOKE ALL ON FUNCTION public.hub_actor() FROM PUBLIC;
GRANT ALL ON FUNCTION public.hub_actor() TO hub;

REVOKE ALL ON FUNCTION public.import_document_repositories(p_repos jsonb) FROM PUBLIC;
GRANT ALL ON FUNCTION public.import_document_repositories(p_repos jsonb) TO hub;

REVOKE ALL ON FUNCTION public.instance_account() FROM PUBLIC;
GRANT ALL ON FUNCTION public.instance_account() TO hub;
GRANT ALL ON FUNCTION public.instance_account() TO hub_web;

REVOKE ALL ON FUNCTION public.instance_account_fixed() FROM PUBLIC;
GRANT ALL ON FUNCTION public.instance_account_fixed() TO hub;

REVOKE ALL ON FUNCTION public.instance_document_link_labels() FROM PUBLIC;
GRANT ALL ON FUNCTION public.instance_document_link_labels() TO hub;

REVOKE ALL ON FUNCTION public.instance_timezone() FROM PUBLIC;
GRANT ALL ON FUNCTION public.instance_timezone() TO hub;
GRANT ALL ON FUNCTION public.instance_timezone() TO hub_web;

REVOKE ALL ON FUNCTION public.seed_instance(p_account text, p_timezone text, p_pending_limit integer, p_pending_ttl interval, p_document_roots text[], p_document_link_labels jsonb) FROM PUBLIC;

REVOKE ALL ON FUNCTION public.sync_repository_documents(snapshot jsonb) FROM PUBLIC;
GRANT ALL ON FUNCTION public.sync_repository_documents(snapshot jsonb) TO hub;

REVOKE ALL ON FUNCTION public.ticket_change_log() FROM PUBLIC;
GRANT ALL ON FUNCTION public.ticket_change_log() TO hub;

REVOKE ALL ON FUNCTION public.ticket_create_log() FROM PUBLIC;
GRANT ALL ON FUNCTION public.ticket_create_log() TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth.account TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth.jwks TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthAccessToken" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthClient" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthClientAssertion" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthClientResource" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthConsent" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthRefreshToken" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."oauthResource" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth.session TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth."user" TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE auth.verification TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.component TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.document TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.edge TO hub;

GRANT ALL ON SEQUENCE graph.edge_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.node TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.ticket TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.ticket_documents TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.tickets TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.edge_v TO hub;

GRANT ALL ON SEQUENCE graph.node_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.node_kind TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.person TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.person_login TO hub;

GRANT ALL ON SEQUENCE graph.person_login_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE graph.rel_shape TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE mailbox.cursors TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE mailbox.imports TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE mailbox.key_review TO hub;

GRANT ALL ON SEQUENCE mailbox.key_review_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE mailbox.messages TO hub;

GRANT ALL ON SEQUENCE mailbox.messages_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.baseline TO hub;

GRANT ALL ON SEQUENCE plan.baseline_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.baseline_live TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.current TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.dirty TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.fever_point TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.points_to_hd TO hub;

GRANT ALL ON SEQUENCE plan.points_to_hd_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.scope TO hub;

GRANT ALL ON SEQUENCE plan.scope_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.ticket_estimate TO hub;

GRANT ALL ON SEQUENCE plan.ticket_estimate_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.ticket_wsjf TO hub;

GRANT ALL ON SEQUENCE plan.ticket_wsjf_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE plan.type_default_estimate TO hub;

GRANT ALL ON SEQUENCE plan.type_default_estimate_id_seq TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.document_ref TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.document_repository TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.documents TO hub;

GRANT SELECT ON TABLE public.instance TO hub;
GRANT SELECT ON TABLE public.instance TO hub_web;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.legacy_imports TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.repository_document_sync_state TO hub;

GRANT SELECT,INSERT,DELETE,UPDATE ON TABLE public.ticket_change TO hub;

GRANT ALL ON SEQUENCE public.ticket_change_id_seq TO hub;

GRANT ALL ON SEQUENCE public.tickets_seq_seq TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA auth GRANT ALL ON SEQUENCES TO hub_web;

ALTER DEFAULT PRIVILEGES IN SCHEMA auth GRANT SELECT,INSERT,DELETE,UPDATE ON TABLES TO hub_web;

ALTER DEFAULT PRIVILEGES IN SCHEMA graph GRANT ALL ON SEQUENCES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA graph GRANT ALL ON FUNCTIONS TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA graph GRANT SELECT,INSERT,DELETE,UPDATE ON TABLES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA mailbox GRANT ALL ON SEQUENCES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA mailbox GRANT ALL ON FUNCTIONS TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA mailbox GRANT SELECT,INSERT,DELETE,UPDATE ON TABLES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA plan GRANT ALL ON SEQUENCES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA plan GRANT ALL ON FUNCTIONS TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA plan GRANT SELECT,INSERT,DELETE,UPDATE ON TABLES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON FUNCTIONS TO hub;

ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT,INSERT,DELETE,UPDATE ON TABLES TO hub;

-- The migration journal is written by the migration role only; hub may read it. The table is absent when the file
-- is applied by something other than sqlx (psql, PGlite).
DO $$
BEGIN
    IF pg_catalog.to_regclass('public._sqlx_migrations') IS NOT NULL THEN
        GRANT SELECT ON public._sqlx_migrations TO hub;
    END IF;
END;
$$;

