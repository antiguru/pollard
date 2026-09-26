#!/bin/bash
# Regenerate the multi-event perf fixtures from one recording.
#
# Needs perf with hardware counters, samply 0.13.1 (emits version 49),
# a samply build from main without period support (emits version 75),
# and a samply build with `--weight-by-period` (samply pull request
# "Record perf event periods in samply import", branch import-period):
#   SAMPLY_V49=/path/to/samply-0.13.1 SAMPLY_V75=/path/to/samply-main \
#   SAMPLY_PERIOD=/path/to/samply-import-period ./regenerate.sh
set -euo pipefail
cd "$(dirname "$0")"
: "${SAMPLY_V49:?set SAMPLY_V49 to a samply 0.13.1 binary}"
: "${SAMPLY_V75:?set SAMPLY_V75 to a samply main binary}"
: "${SAMPLY_PERIOD:?set SAMPLY_PERIOD to a samply binary with --weight-by-period}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

perf record -q -o "$work/multi.data" \
  -e cycles,cache-misses,instructions,branch-misses -F 999 -g \
  -- sh -c 'i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done'

"$SAMPLY_V49" import "$work/multi.data" -s -o "$work/multi_v49.json.gz"
"$SAMPLY_V75" import "$work/multi.data" -s -o "$work/multi_v75.json.gz"
"$SAMPLY_V75" import "$work/multi.data" -s -o "$work/multi_v75.jslb.gz"
"$SAMPLY_PERIOD" import "$work/multi.data" -s --weight-by-period -o "$work/multi_period.json.gz"

# Replace host-identifying strings with same-length placeholders, so
# JSLB slab offsets stay valid.
host=$(uname -n)
release=$(uname -r)
for f in multi_v49.json.gz multi_v75.json.gz multi_v75.jslb.gz multi_period.json.gz; do
  python3 - "$work/$f" "$f" "$host" "$release" "$HOME" <<'EOF'
import gzip, sys
src, dst, host, release, home = sys.argv[1:]
data = gzip.open(src).read()
for secret in (host, release, home):
    data = data.replace(secret.encode(), b"x" * len(secret.encode()))
assert host.encode() not in data and release.encode() not in data and home.encode() not in data
with gzip.GzipFile(filename=dst, mode="wb", mtime=0) as out:
    out.write(data)
EOF
done
