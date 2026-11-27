#!/bin/sh
# Generates deploy/chart: aspire publish, then the deploy/overlay files are merged into the templates of the same name,
# and deploy/overlay/values.yaml (if present) into the generated values.yaml.
# Helm templating that a YAML patch cannot express (step 2.10 of the factory plan):
#   '{{= X }}'   a quoted marker string becomes {{ X }} without quotes (unquoted value, toYaml, include);
#   - '{{=- X }}'  a whole list item becomes the line {{- X }} (if, end, block include);
#   x-helm-if: X  a top-level key of the patch wraps the whole template in {{- if X }} ... {{- end }};
#   deploy/overlay/templates/*.tpl are copied as is;
#   nameOverride in deploy/overlay/values.yaml: the Aspire {{ .Chart.Name }} labels read it.
# Without markers, .tpl files and nameOverride the chart is the same as before them: the gate takes this script from main (lesson L26).
# The gate runs this same script and requires an empty git diff. CHECK_SH_ROOT: root of the repo whose chart to generate,
# if the script does not live in it (the .tekton/pr.yaml gate takes it from main, while the PR head generates the chart).
set -eu
cd "${CHECK_SH_ROOT:-$(dirname "$0")/..}"
aspire="${ASPIRE:-.tools/aspire}"
yq="${YQ:-mise exec yq@4.54.3 -- yq}"
die() { echo "tools/chart.sh: $*" >&2; exit 1; }
containers='.spec.template.spec.containers'

rm -rf deploy/chart
"$aspire" publish -o deploy/chart --non-interactive --nologo >/dev/null

# values.yaml has no kind: it is not a template and is merged separately after the loop.
for f in $(cd deploy/overlay && find . -type f -name '*.yaml' ! -path ./values.yaml | sort); do
  o="deploy/overlay/$f" c="deploy/chart/$f"
  # A patch without a target means AppHost renamed the resource: it must not be skipped silently.
  [ -f "$c" ] || die "overlay $f: the generated chart has no such file"
  # A patch declares its kind on the first line; it is not merged into the chart (del below) but compared with the generated one.
  want="$($yq '.kind // ""' "$o")" got="$($yq '.kind // ""' "$c")"
  [ -n "$want" ] || die "overlay $f: no kind"
  [ "$want" = "$got" ] || die "overlay $f: written for kind $want, but Aspire generated $got"
  # The tls array is merged by index too: hosts would land in someone else's entry (for example, one with secretName from withIngressTlsAuto).
  if [ "$($yq '.spec.tls // [] | length' "$o")" != 0 ]; then
    [ "$($yq '.spec.tls // [] | length' "$c")" = 0 ] || die "overlay $f: Aspire generated its own spec.tls, merging by index would break it"
  fi
  # *d merges arrays by index. So that a patch does not silently land on someone else's element:
  # every container of the patch has a name, the container names after the merge are the same as before it,
  # and the patch does not bring env where Aspire already generated its own env list.
  [ "$($yq "[$containers[]? | select(has(\"name\") | not)] | length" "$o")" = 0 ] || die "overlay $f: container without name"
  if [ "$($yq "[$containers[]? | select(has(\"env\"))] | length" "$o")" != 0 ]; then
    [ "$($yq "[$containers[]? | select(has(\"env\"))] | length" "$c")" = 0 ] || die "overlay $f: Aspire generated its own env, merging by index would overwrite it"
  fi
  before="$($yq "[$containers[]?.name] | join(\",\")" "$c")"
  OVERLAY="$o" $yq -i '. *d (load(strenv(OVERLAY)) | del(.kind) | del(.["x-helm-if"]))' "$c"
  after="$($yq "[$containers[]?.name] | join(\",\")" "$c")"
  # A generated template without containers (addManifest, for example Job hub-db-migrate): the patch cannot
  # land on someone else's index, the containers are entirely its own. Otherwise the set and order do not change.
  [ -z "$before" ] || [ "$before" = "$after" ] || die "overlay $f: containers in the chart [$before], after the merge [$after]: order or set changed"
  # After all yq calls: a wrapped template is no longer YAML.
  cond="$($yq '.["x-helm-if"] // ""' "$o")"
  if [ -n "$cond" ]; then
    { echo "{{- if $cond }}"; cat "$c"; echo "{{- end }}"; } >"$c.new"
    mv "$c.new" "$c"
  fi
done

# Values over the generated ones: maps merge deeply, lists and scalars of the patch replace the generated ones entirely
# (merging lists by index would silently break someone else's element). A key Aspire did not generate lands too:
# this is how values for the overlay templates reach the chart.
if [ -f deploy/overlay/values.yaml ]; then
  [ -f deploy/chart/values.yaml ] || die "overlay values.yaml: the generated chart has no values.yaml"
  OVERLAY=deploy/overlay/values.yaml $yq -i '. * load(strenv(OVERLAY))' deploy/chart/values.yaml
fi

name='{{ .Chart.Name }}'
if [ -f deploy/overlay/values.yaml ] && [ "$($yq 'has("nameOverride")' deploy/overlay/values.yaml)" = true ]; then
  name='{{ .Values.nameOverride | default .Chart.Name }}'
fi
for f in $(cd deploy/overlay && find . -type f -name '*.tpl' | sort); do
  cp "deploy/overlay/$f" "deploy/chart/$f"
done
# Markers into Helm text. First the whole list item, then a string inside a line.
for f in $(find deploy/chart/templates -type f -name '*.yaml' | sort); do
  sed -E \
    -e "s/^ *- '\{\{=(-[^']*)\}\}' *\$/{{\1}}/" \
    -e "s/'\{\{=([^']*)\}\}'/{{\1}}/g" \
    -e 's/"\{\{=([^"]*)\}\}"/{{\1}}/g' \
    -e "s/\\{\\{ \\.Chart\\.Name \\}\\}/$name/g" \
    "$f" >"$f.new"
  mv "$f.new" "$f"
  ! grep -n '{{=' "$f" || die "$f: marker {{= not resolved (quotes around it?)"
done
