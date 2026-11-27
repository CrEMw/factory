-- Canonical catalog of the hub schema (step 2.12b of the factory plan): one text line per object property, sorted.
-- Two databases have the same schema when pg_temp.schema_catalog() gives the same lines. Used by
-- tools/test-pg-schema.sh (the migration chain against services/hub/baseline on empty databases) and by the clone
-- rehearsal of 0028 (md5 of the lines against the md5 of the baseline).
--   \i services/hub/pg-schema.sql
--   SELECT line FROM pg_temp.schema_catalog() AS line ORDER BY line COLLATE "C";
--   SELECT md5(string_agg(line, E'\n' ORDER BY line COLLATE "C")) FROM pg_temp.schema_catalog() AS line;
-- Order by COLLATE "C" (bytes): the default collation of the database must not change the order or the md5.
-- Covered: schemas, extensions, relations (kind, persistence, options, row security), columns in order (type,
-- not null, identity, generated, collation, default), constraints, indexes, triggers, rules, policies, views, sequences,
-- user types, functions (signature, result, language, volatility, security, strictness, leakproofness, parallel
-- safety, cost, rows, config, body byte for byte), comments, grants (tables, columns, sequences, functions, schemas,
-- types) and default privileges. Not covered: owners and grantors (the migration role differs per installation; the
-- owner of an object shows as OWNER in its grants, the role that runs the query as CURRENT in default privileges), data,
-- sequence values, the sqlx journal public._sqlx_migrations, temporary schemas and extension members.
-- The search path of the functions is pg_catalog only, so every deparsed name outside pg_catalog is schema-qualified.

-- Grants without grantors, sorted: the owner as OWNER, PUBLIC for grantee 0, '*' for WITH GRANT OPTION.
CREATE OR REPLACE FUNCTION pg_temp.schema_acl(p_acl aclitem[], p_owner oid) RETURNS text
LANGUAGE sql STABLE SET search_path = pg_catalog AS $acl$
    SELECT coalesce(string_agg(DISTINCT g COLLATE "C", ',' ORDER BY g COLLATE "C"), '')
    FROM (SELECT CASE WHEN e.grantee = 0 THEN 'PUBLIC' WHEN e.grantee = p_owner THEN 'OWNER' ELSE e.grantee::regrole::text END
                 || ':' || e.privilege_type || CASE WHEN e.is_grantable THEN '*' ELSE '' END AS g
          FROM aclexplode(p_acl) e) x
$acl$;

-- A missing ACL (NULL) is compared as the default one (acldefault): pg_dump writes no grant that equals the default.
CREATE OR REPLACE FUNCTION pg_temp.schema_catalog() RETURNS SETOF text
LANGUAGE sql STABLE SET search_path = pg_catalog AS $catalog$
WITH
ns AS (
    SELECT oid, nspname FROM pg_namespace
    WHERE nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND nspname NOT LIKE 'pg\_temp\_%' AND nspname NOT LIKE 'pg\_toast\_temp\_%'
),
ext AS (SELECT objid, classid FROM pg_depend WHERE deptype = 'e'),
rel AS (
    SELECT c.*, n.nspname FROM pg_class c JOIN ns n ON n.oid = c.relnamespace
    WHERE NOT (n.nspname = 'public' AND c.relname = '_sqlx_migrations')
      AND NOT EXISTS (SELECT 1 FROM pg_index i JOIN pg_class t ON t.oid = i.indrelid
                      WHERE i.indexrelid = c.oid AND t.relname = '_sqlx_migrations' AND t.relnamespace = 'public'::regnamespace)
      AND NOT EXISTS (SELECT 1 FROM ext e WHERE e.classid = 'pg_class'::regclass AND e.objid = c.oid)
),
fn AS (
    SELECT p.*, n.nspname FROM pg_proc p JOIN ns n ON n.oid = p.pronamespace
    WHERE NOT EXISTS (SELECT 1 FROM ext e WHERE e.classid = 'pg_proc'::regclass AND e.objid = p.oid)
),
typ AS (
    SELECT t.*, n.nspname FROM pg_type t JOIN ns n ON n.oid = t.typnamespace
    WHERE t.typtype IN ('e', 'd', 'r', 'm') OR (t.typtype = 'c' AND (SELECT relkind FROM pg_class WHERE oid = t.typrelid) = 'c')
)
SELECT line FROM (
    SELECT 'schema ' || n.nspname || ' acl=' || pg_temp.schema_acl(coalesce(s.nspacl, acldefault('n', s.nspowner)), s.nspowner)
    FROM ns n JOIN pg_namespace s ON s.oid = n.oid
    UNION ALL
    SELECT 'extension ' || x.extname || ' version=' || x.extversion || ' schema=' || x.extnamespace::regnamespace::text
    FROM pg_extension x WHERE x.extname <> 'plpgsql'
    UNION ALL
    SELECT 'relation ' || r.nspname || '.' || r.relname || ' kind=' || r.relkind::text || ' persistence=' || r.relpersistence::text
        || ' options=' || coalesce(array_to_string(r.reloptions, ','), '') || ' rls=' || r.relrowsecurity || '/' || r.relforcerowsecurity
        || ' acl=' || CASE WHEN r.relkind IN ('i', 'I') THEN '' ELSE pg_temp.schema_acl(coalesce(r.relacl,
               acldefault((CASE WHEN r.relkind = 'S' THEN 's' ELSE 'r' END)::"char", r.relowner)), r.relowner) END
    FROM rel r
    UNION ALL
    SELECT 'column ' || r.nspname || '.' || r.relname || ' #' || lpad(row_number() OVER (PARTITION BY r.oid ORDER BY a.attnum)::text, 3, '0')
        || ' ' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod) || CASE WHEN a.attnotnull THEN ' not null' ELSE '' END
        || CASE WHEN a.attidentity <> '' THEN ' identity=' || a.attidentity::text ELSE '' END
        || CASE WHEN a.attgenerated <> '' THEN ' generated=' || a.attgenerated::text ELSE '' END
        || CASE WHEN a.attcollation <> 0 AND a.attcollation <> (SELECT typcollation FROM pg_type WHERE oid = a.atttypid)
                THEN ' collate=' || a.attcollation::regcollation::text ELSE '' END
        || coalesce(' default=' || pg_get_expr(d.adbin, d.adrelid), '')
        || coalesce(' acl=' || pg_temp.schema_acl(a.attacl, r.relowner), '')
    FROM rel r JOIN pg_attribute a ON a.attrelid = r.oid AND a.attnum > 0 AND NOT a.attisdropped
    LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
    WHERE r.relkind IN ('r', 'p', 'v', 'm', 'f', 'c')
    UNION ALL
    SELECT 'constraint ' || coalesce(r.nspname || '.' || r.relname, t.nspname || '.' || t.typname) || ' ' || c.conname
        || ' type=' || c.contype::text || ' deferrable=' || c.condeferrable || '/' || c.condeferred || ' valid=' || c.convalidated
        || ' noinherit=' || c.connoinherit || ' ' || pg_get_constraintdef(c.oid)
    FROM pg_constraint c LEFT JOIN rel r ON r.oid = c.conrelid LEFT JOIN typ t ON t.oid = c.contypid
    WHERE r.oid IS NOT NULL OR t.oid IS NOT NULL
    UNION ALL
    SELECT 'index ' || pg_get_indexdef(i.indexrelid) || ' valid=' || i.indisvalid || ' clustered=' || i.indisclustered
        || ' replident=' || i.indisreplident
    FROM pg_index i JOIN rel r ON r.oid = i.indexrelid
    UNION ALL
    SELECT 'trigger ' || pg_get_triggerdef(g.oid) || ' enabled=' || g.tgenabled::text
    FROM pg_trigger g JOIN rel r ON r.oid = g.tgrelid WHERE NOT g.tgisinternal
    UNION ALL
    SELECT 'rule ' || pg_get_ruledef(w.oid) FROM pg_rewrite w JOIN rel r ON r.oid = w.ev_class WHERE w.rulename <> '_RETURN'
    UNION ALL
    SELECT 'policy ' || r.nspname || '.' || r.relname || ' ' || p.polname || ' cmd=' || p.polcmd::text || ' permissive=' || p.polpermissive
        || ' roles=' || (SELECT string_agg(y, ',' ORDER BY y COLLATE "C") FROM (SELECT CASE WHEN x = 0 THEN 'PUBLIC' ELSE x::regrole::text END AS y FROM unnest(p.polroles) x) z)
        || coalesce(' using=' || pg_get_expr(p.polqual, p.polrelid), '') || coalesce(' check=' || pg_get_expr(p.polwithcheck, p.polrelid), '')
    FROM pg_policy p JOIN rel r ON r.oid = p.polrelid
    UNION ALL
    SELECT 'view ' || r.nspname || '.' || r.relname || ' ' || replace(replace(pg_get_viewdef(r.oid), '\', '\\'), E'\n', '\n')
    FROM rel r WHERE r.relkind IN ('v', 'm')
    UNION ALL
    SELECT 'sequence ' || r.nspname || '.' || r.relname || ' type=' || format_type(s.seqtypid, NULL) || ' start=' || s.seqstart
        || ' increment=' || s.seqincrement || ' min=' || s.seqmin || ' max=' || s.seqmax || ' cache=' || s.seqcache
        || ' cycle=' || s.seqcycle
        || coalesce(' owned_by=' || (SELECT d.refobjid::regclass::text || '.' || a.attname || '/' || d.deptype::text
                                    FROM pg_depend d JOIN pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid
                                    WHERE d.classid = 'pg_class'::regclass AND d.objid = r.oid
                                      AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i')), '')
    FROM rel r JOIN pg_sequence s ON s.seqrelid = r.oid
    UNION ALL
    SELECT 'type ' || t.nspname || '.' || t.typname || ' type=' || t.typtype::text
        || coalesce(' labels=' || (SELECT string_agg(e.enumlabel, ',' ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid), '')
        || CASE WHEN t.typtype = 'd' THEN ' base=' || format_type(t.typbasetype, t.typtypmod) || ' not null=' || t.typnotnull
                || coalesce(' default=' || t.typdefault, '') ELSE '' END
        || ' acl=' || pg_temp.schema_acl(coalesce(t.typacl, acldefault('T', t.typowner)), t.typowner)
    FROM typ t
    UNION ALL
    SELECT 'function ' || p.oid::regprocedure::text || ' kind=' || p.prokind::text
        || ' args=' || pg_get_function_arguments(p.oid) || ' result=' || coalesce(pg_get_function_result(p.oid), '')
        || ' language=' || (SELECT lanname FROM pg_language WHERE oid = p.prolang) || ' volatility=' || p.provolatile::text
        || ' security_definer=' || p.prosecdef || ' strict=' || p.proisstrict || ' leakproof=' || p.proleakproof
        || ' parallel=' || p.proparallel::text || ' cost=' || p.procost || ' rows=' || p.prorows
        || ' config=' || coalesce(array_to_string(p.proconfig, ','), '')
        || ' acl=' || pg_temp.schema_acl(coalesce(p.proacl, acldefault('f', p.proowner)), p.proowner)
        || ' body=' || coalesce(replace(replace(p.prosrc, '\', '\\'), E'\n', '\n'), '')
        || coalesce(' sqlbody=' || replace(replace(pg_get_function_sqlbody(p.oid), '\', '\\'), E'\n', '\n'), '')
    FROM fn p
    UNION ALL
    SELECT 'comment ' || pg_describe_object(d.classoid, d.objoid, d.objsubid) || ' = '
        || replace(replace(d.description, '\', '\\'), E'\n', '\n')
    FROM pg_description d
    WHERE (d.classoid = 'pg_namespace'::regclass AND d.objoid IN (SELECT oid FROM ns))
       OR (d.classoid = 'pg_class'::regclass AND d.objoid IN (SELECT oid FROM rel))
       OR (d.classoid = 'pg_proc'::regclass AND d.objoid IN (SELECT oid FROM fn))
       OR (d.classoid = 'pg_type'::regclass AND d.objoid IN (SELECT oid FROM typ))
       OR (d.classoid = 'pg_constraint'::regclass AND d.objoid IN (SELECT c.oid FROM pg_constraint c
               WHERE c.conrelid IN (SELECT oid FROM rel) OR c.contypid IN (SELECT oid FROM typ)))
       OR (d.classoid = 'pg_trigger'::regclass AND d.objoid IN (SELECT g.oid FROM pg_trigger g WHERE g.tgrelid IN (SELECT oid FROM rel)))
       OR (d.classoid = 'pg_policy'::regclass AND d.objoid IN (SELECT p.oid FROM pg_policy p WHERE p.polrelid IN (SELECT oid FROM rel)))
       OR (d.classoid = 'pg_rewrite'::regclass AND d.objoid IN (SELECT w.oid FROM pg_rewrite w WHERE w.ev_class IN (SELECT oid FROM rel)))
       OR (d.classoid = 'pg_extension'::regclass)
    UNION ALL
    SELECT 'default privileges ' || CASE WHEN a.defaclrole = current_user::regrole THEN 'CURRENT' ELSE a.defaclrole::regrole::text END
        || ' schema=' || coalesce((SELECT nspname FROM pg_namespace WHERE oid = a.defaclnamespace), '')
        || ' objtype=' || a.defaclobjtype::text || ' acl=' || pg_temp.schema_acl(a.defaclacl, a.defaclrole)
    FROM pg_default_acl a
) x(line)
$catalog$;
