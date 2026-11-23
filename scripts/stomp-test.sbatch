#!/bin/bash
#SBATCH --job-name=stomp-test
#SBATCH --time=02:00:00
#SBATCH --cpus-per-task=8
#SBATCH --mem=8G
#SBATCH --output=stomp-test-%j.out
# If your cluster needs these, delete one '#' and fill them in:
##SBATCH --partition=general
##SBATCH --account=your-account
#
# Tries stomp's local archiving on a sample of your real GeoTIFF and CSV files.
#
#     sbatch scripts/stomp-test.sbatch ~/some_project ~/another_project
#
# The directories you name are only read. A random sample of *.tif/*.tiff
# files, and another of *.csv files, is copied into a test folder, and only
# those copies are archived. For each file type and compression level:
#
#     estimate → archive → verify --deep → restore → compare with the copies
#
# stomp runs with its own config and data directory, so the test doesn't
# appear in your usage reports or archive list. The test folder is removed
# at the end (KEEP=1 keeps it).
#
# Results go to the directory you ran sbatch from:
#     stomp-test-<job>.out   everything, with a summary at the end
#     stomp-test-<job>.tsv   one row per file type and level, for spreadsheets
#
# Settings, as environment variables (e.g. LEVELS="3 9 19" sbatch ...):
#     STOMP       the stomp binary               [stomp on PATH, else ~/bin/stomp]
#     TYPES       file types to test             ["geotiff csv"]
#     SAMPLE_GB   sample size per file type, GB  [5]
#     MAX_FILES   most files per file type       [20000]
#     LEVELS      zstd levels to compare         ["3", stomp's default]
#     CHUNK_SIZE  stomp chunk size               [2GiB, so a sample spans several chunks]
#     WORK        test folder; needs free space of about 2 × SAMPLE_GB
#                                                [~/stomp-test-<job>, on the filesystem you'll archive]
#     KEEP        1 keeps the test folder        [0]
#     CANDIDATES  matching files to sample from  [200000, the first ones found]
#
# Finding files walks the directories you name, so name a few project
# directories rather than all of ~.

set -uo pipefail

JOB=${SLURM_JOB_ID:-local-$$}
OUT=${SLURM_SUBMIT_DIR:-$PWD}
TSV=$OUT/stomp-test-$JOB.tsv
STOMP=${STOMP:-$(command -v stomp || echo "$HOME/bin/stomp")}
TYPES=${TYPES:-geotiff csv}
SAMPLE_GB=${SAMPLE_GB:-5}
MAX_FILES=${MAX_FILES:-20000}
LEVELS=${LEVELS:-3}
CHUNK_SIZE=${CHUNK_SIZE:-2GiB}
WORK=${WORK:-$HOME/stomp-test-$JOB}
KEEP=${KEEP:-0}
CANDIDATES=${CANDIDATES:-200000}
CPUS=${SLURM_CPUS_PER_TASK:-$(nproc)}

log() { printf '%(%H:%M:%S)T  %s\n' -1 "$*"; }
section() { printf '\n==== %s\n\n' "$*"; }
die() {
    echo "stomp-test: $*" >&2
    exit 2
}
human() { numfmt --to=iec-i --suffix=B --format=%.1f "$1"; }
clock() { date +%s.%N; }
since() { awk -v a="$1" -v b="$(clock)" 'BEGIN { printf "%.1f", b - a }'; }

# ---- Checks before touching anything

[ $# -gt 0 ] || die "usage: sbatch $0 DIR [DIR...]   (directories to sample files from)"
[ -x "$STOMP" ] || die "can't find stomp at $STOMP; set STOMP=/path/to/stomp"
SOURCES=()
for d in "$@"; do
    [ -d "$d" ] || die "$d is not a directory"
    SOURCES+=("$(realpath -- "$d")")
done
for t in $TYPES; do
    case $t in
    geotiff | csv) ;;
    *) die "unknown file type '$t' in TYPES (use geotiff and/or csv)" ;;
    esac
done
[ -e "$WORK" ] && die "$WORK already exists; remove it or set WORK to another folder"
BUDGET=$(awk -v g="$SAMPLE_GB" 'BEGIN { printf "%d", g * 1e9 }')
[ "$BUDGET" -gt 0 ] || die "SAMPLE_GB must be more than 0"

mkdir -p -- "$WORK" || die "can't create $WORK"
WORK=$(realpath -- "$WORK")
cleanup() {
    if [ "$KEEP" = 1 ]; then
        log "Kept the test folder $WORK"
    else
        rm -rf -- "$WORK"
    fi
}
trap cleanup EXIT

# ---- An isolated stomp: its own config, grants and data directory

mkdir -p -- "$WORK/state" "$WORK/checks"
CONFIG=$WORK/config.toml
{
    echo '[general]'
    echo "roots = ['$WORK/data']"
    echo '[scan]'
    echo "max_threads = $CPUS"
    for t in $TYPES; do
        mkdir -p -- "$WORK/data/$t"
        echo '[[grant]]'
        echo "id = 'TEST-${t^^}'"
        echo "dirs = ['$WORK/data/$t']"
    done
} >"$CONFIG"
export XDG_DATA_HOME=$WORK/state
st() { "$STOMP" --config "$CONFIG" "$@"; }

echo "stomp test, job $JOB, $(date '+%Y-%m-%d %H:%M %Z')"
echo
echo "stomp      $("$STOMP" --version 2>&1)   ($STOMP)"
echo "node       $(hostname), $CPUS CPUs,$(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2)"
echo "test dir   $WORK   (filesystem: $(stat -f -c %T "$WORK"))"
echo "sources    ${SOURCES[*]}"
echo "settings   TYPES=\"$TYPES\" SAMPLE_GB=$SAMPLE_GB MAX_FILES=$MAX_FILES LEVELS=\"$LEVELS\" CHUNK_SIZE=$CHUNK_SIZE"

section "stomp doctor (destinations skipped)"
st doctor --offline || true

# ---- Sampling

# Prints a random sample of one file type from the sources, NUL-separated,
# within the size and count limits.
pick() {
    local -a match
    case $1 in
    geotiff) match=(-iname '*.tif' -o -iname '*.tiff') ;;
    csv) match=(-iname '*.csv') ;;
    esac
    local total=0 count=0 size path
    find "${SOURCES[@]}" \( -path "$WORK" -o -name .stomp-archive -o -name .snapshot \) -prune \
        -o -type f \( "${match[@]}" \) -size +0 -readable -printf '%s\t%p\0' 2>/dev/null |
        head -z -n "$CANDIDATES" | shuf -z |
        while IFS=$'\t' read -r -d '' size path; do
            ((total + size > BUDGET)) && continue
            printf '%s\0' "$path"
            total=$((total + size))
            count=$((count + 1))
            ((count >= MAX_FILES)) && break
        done
}

# Copies a sample into the test folder, keeping modes and times, and prints
# how many files were copied.
copy_sample() {
    local dest=$WORK/data/$1 i=0 path sub
    while IFS= read -r -d '' path; do
        sub=$dest/$(printf '%03d' $((i / 1000)))
        ((i % 1000 == 0)) && mkdir -p -- "$sub"
        if cp --preserve=mode,timestamps -- "$path" "$sub/$(printf '%05d' "$i")_${path##*/}"; then
            i=$((i + 1))
        fi
    done < <(pick "$1")
    echo "$i"
}

# Records each file's path, size, mode and mtime (whole seconds), and its
# SHA-256, ignoring stomp's own files.
fingerprint() {
    (
        cd -- "$1" || exit 1
        find . -name .stomp-archive -prune -o -type f ! -name STOMP_ARCHIVED.txt -printf '%P\t%s\t%m\t%T@\n' |
            awk -F'\t' -v OFS='\t' '{ sub(/\..*/, "", $4); print }' | LC_ALL=C sort >"$2.meta"
        find . -name .stomp-archive -prune -o -type f ! -name STOMP_ARCHIVED.txt -print0 |
            xargs -0 -r -P "$CPUS" -n 64 sha256sum | LC_ALL=C sort -k2 >"$2.sha256"
    )
}

TIFF_PY=$(
    cat <<'EOF'
import collections, os, struct, sys

NAMES = {1: "none", 2: "CCITT", 3: "CCITT", 4: "CCITT", 5: "LZW", 6: "old JPEG",
         7: "JPEG", 8: "Deflate", 32773: "PackBits", 32946: "Deflate", 34712: "JPEG 2000",
         34887: "LERC", 34925: "LZMA", 50000: "ZSTD", 50001: "WebP"}

def scheme(path):
    """The Compression tag of a TIFF's first image (classic or BigTIFF)."""
    with open(path, "rb") as f:
        head = f.read(16)
        order = {b"II": "<", b"MM": ">"}.get(head[:2])
        if order is None or len(head) < 8:
            return "not a TIFF"
        magic = struct.unpack(order + "H", head[2:4])[0]
        if magic == 42:
            ifd, count, entry, value = struct.unpack(order + "I", head[4:8])[0], "H", 12, 8
        elif magic == 43 and len(head) == 16:
            ifd, count, entry, value = struct.unpack(order + "Q", head[8:16])[0], "Q", 20, 12
        else:
            return "not a TIFF"
        f.seek(ifd)
        n = struct.unpack(order + count, f.read(struct.calcsize(count)))[0]
        if n > 4096:
            return "unusual TIFF"
        table = f.read(n * entry)
        for i in range(n):
            e = table[i * entry:(i + 1) * entry]
            if len(e) == entry and struct.unpack(order + "H", e[:2])[0] == 259:
                code = struct.unpack(order + "H", e[value:value + 2])[0]
                return NAMES.get(code, "code %d" % code)
        return "none"

files, sizes = collections.Counter(), collections.Counter()
for p in sys.stdin.buffer.read().split(b"\0"):
    if p:
        try:
            s = scheme(p)
        except (OSError, struct.error):
            s = "unreadable"
        files[s] += 1
        sizes[s] += os.path.getsize(p)
total = sum(sizes.values()) or 1
for s, b in sizes.most_common():
    print("  %-12s %7d files   %5.1f%% of the bytes" % (s, files[s], 100 * b / total))
EOF
)

# Shows how the sampled GeoTIFFs are compressed inside: files that already
# use LZW, Deflate, JPEG or ZSTD shrink little further.
tiff_compression() {
    command -v python3 >/dev/null || return 0
    log "Compression inside the sampled GeoTIFFs:"
    find "$1" -type f -print0 | python3 -I -c "$TIFF_PY"
}

# ---- One test: estimate, archive, verify, restore, compare

run() {
    local type=$1 level=$2 dir=$WORK/data/$1 base=$WORK/checks/$1 tag=$WORK/checks/$1-$2
    local result=ok est="" t id stored=0 chunks=0 archive_s="" verify_s="" restore_s="" left files bytes
    section "$type, zstd level $level"

    log "Estimating"
    if st archive "$dir" --estimate --level "$level" --chunk-size "$CHUNK_SIZE" --format json >"$tag.estimate.json"; then
        est=$(sed -n 's/^ *"ratio": *\([0-9.eE+-]*\).*/\1/p' "$tag.estimate.json" | head -n 1)
    fi
    [ -n "$est" ] && log "Estimated: $(awk -v r="$est" 'BEGIN { printf "%.0f", r * 100 }')% of the original size"

    log "Archiving"
    t=$(clock)
    st archive "$dir" --to local --here --yes --level "$level" --chunk-size "$CHUNK_SIZE" || result="archive failed"
    archive_s=$(since "$t")
    id=$(ls -1t -- "$dir/.stomp-archive" 2>/dev/null | head -n 1)
    if [ -n "$id" ]; then
        read -r stored chunks < <(find "$dir/.stomp-archive/$id" -name 'chunk-*' -printf '%s\n' |
            awk '{ s += $1; n++ } END { print s + 0, n + 0 }')
    fi
    left=$(find "$dir" -name .stomp-archive -prune -o -type f ! -name STOMP_ARCHIVED.txt -print | wc -l)
    [ "$left" -gt 0 ] && log "$left files were left in place rather than archived"

    if [ "$result" = ok ]; then
        log "Verifying every file in the archive"
        t=$(clock)
        st archives verify "$dir" --deep || result="verify failed"
        verify_s=$(since "$t")
    fi

    log "Restoring"
    t=$(clock)
    if ! st restore "$dir" --here --yes; then
        [ "$result" = ok ] && result="restore failed"
    fi
    restore_s=$(since "$t")

    log "Comparing the restored files with the copies"
    fingerprint "$dir" "$tag.after"
    if ! cmp -s "$base.before.meta" "$tag.after.meta"; then
        [ "$result" = ok ] && result="sizes, modes or times differ"
        diff "$base.before.meta" "$tag.after.meta" | head -n 20
    fi
    if ! cmp -s "$base.before.sha256" "$tag.after.sha256"; then
        result="CONTENTS DIFFER"
        diff "$base.before.sha256" "$tag.after.sha256" | head -n 20
    fi
    log "Result: $result"

    read -r files bytes < <(awk -F'\t' '{ s += $2 } END { print NR, s + 0 }' "$base.before.meta")
    awk -v OFS='\t' -v type="$type" -v level="$level" -v files="$files" -v bytes="$bytes" \
        -v stored="$stored" -v est="$est" -v chunks="$chunks" -v a="$archive_s" -v v="$verify_s" \
        -v r="$restore_s" -v left="$left" -v result="$result" 'BEGIN {
            pct = bytes > 0 ? sprintf("%.1f", 100 * stored / bytes) : ""
            estp = est != "" ? sprintf("%.1f", 100 * est) : ""
            mbs = a > 0 ? sprintf("%.1f", bytes / 1e6 / a) : ""
            print type, level, files, bytes, stored, pct, estp, chunks, a, mbs, v, r, left, result
        }' >>"$TSV"
    [ "$result" = ok ]
}

# ---- Main

printf 'type\tlevel\tfiles\toriginal_bytes\tstored_bytes\tstored_pct\testimated_pct\tchunks\tarchive_s\tarchive_MB_per_s\tverify_s\trestore_s\tfiles_left_in_place\tresult\n' >"$TSV"

for type in $TYPES; do
    section "Sampling $type files"
    log "Looking for $type files and copying a sample of up to $SAMPLE_GB GB, $MAX_FILES files"
    n=$(copy_sample "$type")
    if [ "$n" -eq 0 ]; then
        log "No readable $type files found in the sources; skipping"
        continue
    fi
    fingerprint "$WORK/data/$type" "$WORK/checks/$type.before"
    log "Copied $n files, $(human "$(awk -F'\t' '{ s += $2 } END { print s + 0 }' "$WORK/checks/$type.before.meta")")"
    [ "$type" = geotiff ] && tiff_compression "$WORK/data/$type"

    for level in $LEVELS; do
        # After a failure the copies may not be intact, so stop testing this type.
        run "$type" "$level" || break
    done
    # Free the space before the next type.
    [ "$KEEP" = 1 ] || find "$WORK/data/$type" -mindepth 1 -delete
done

section "Summary"
awk -F'\t' '
    function size(b,   u, i) {
        split("B KiB MiB GiB TiB PiB", u, " ")
        for (i = 1; b >= 1024 && i < 6; i++) b /= 1024
        return sprintf(i == 1 ? "%d %s" : "%.1f %s", b, u[i])
    }
    function dur(s) { return s == "" ? "—" : (s < 90 ? sprintf("%.0fs", s) : sprintf("%dm%02ds", s / 60, s % 60)) }
    NR == 1 { next }
    {
        rows++
        printf "%-8s level %-3s %6d files  %10s → %-10s", $1, $2, $3, size($4), size($5)
        if ($6 != "") printf "  stored %5.1f%%, saves %-10s", $6, size($4 - $5)
        if ($7 != "") printf "  (estimated %.0f%%)", $7
        printf "\n         archive %s (%s MB/s), verify %s, restore %s, %s chunk(s)   %s\n\n", dur($9), $10 == "" ? "—" : $10, dur($11), dur($12), $8, $14
    }
    END { if (!rows) print "Nothing was tested." }
' "$TSV"

echo "Results for spreadsheets: $TSV"
if [ -n "${SLURM_JOB_ID:-}" ]; then
    echo "Memory and CPU use, once the job ends: seff $SLURM_JOB_ID"
fi
echo "How much of each file type a grant holds: stomp scan, then stomp report grant <ID> (File types)"
# Exit non-zero if any test failed, so Slurm records the job as failed.
awk -F'\t' 'NR > 1 && $14 != "ok" { bad = 1 } END { exit bad }' "$TSV"
