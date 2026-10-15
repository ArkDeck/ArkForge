#!/bin/sh
# Exercise both shipped matchers with an applet set that deliberately lacks tr.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/arkforge-preboot-holder.XXXXXXXX")
trap 'rm -f -- "$scratch/cmdline" "$scratch/toybox" "$scratch/matcher"; rmdir -- "$scratch"' EXIT
cat > "$scratch/toybox" <<'SH'
#!/bin/sh
case "$1" in
    od) shift; exec od "$@" ;;
    *) exit 127 ;;
esac
SH
chmod +x "$scratch/toybox"

for source in \
    "$root/crates/arkforge-artifact/src/preboot-init.sh" \
    "$root/crates/arkforge-standalone/src/hdc_control/preboot.rs"
do
    {
        printf '#!/bin/sh\nset -eu\nt=$2\nfor p in "$1"; do\n'
        awk '
            /^    bytes=/ { copy=1 }
            copy {
                gsub(/\/proc\/\$holder\/cmdline/, "\"$p/cmdline\"")
                if ($0 ~ /^    if test /) {
                    sub(/; then.*/, "; then exit 0; else exit 1; fi")
                    print
                    found=1
                    exit
                }
                print
            }
            END { if (!found) exit 1 }
        ' "$source"
        printf 'done\nexit 1\n'
    } > "$scratch/matcher"

    printf '%s\000' /af-preboot/toybox sleep 600 > "$scratch/cmdline"
    sh "$scratch/matcher" "$scratch" "$scratch/toybox"
    for invalid in suffix extra path missing-nul spaces empty oversized missing
    do
        case "$invalid" in
            suffix) printf '%s\000' /af-preboot/toybox sleep 6000 ;;
            extra) printf '%s\000' /af-preboot/toybox sleep 600 extra ;;
            path) printf '%s\000' /bin/toybox sleep 600 ;;
            missing-nul) printf '%s\000' /af-preboot/toybox sleep; printf 600 ;;
            spaces) printf '%s\000' '/af-preboot/toybox sleep 600' ;;
            empty|missing) : ;;
            oversized)
                printf '%s\000' /af-preboot/toybox sleep 600
                dd if=/dev/zero bs=1024 count=1024 2>/dev/null ;;
        esac > "$scratch/cmdline"
        if test "$invalid" = missing; then rm -- "$scratch/cmdline"; fi
        if sh "$scratch/matcher" "$scratch" "$scratch/toybox"; then
            printf 'Incorrectly accepted %s in %s\n' "$invalid" "$source" >&2
            exit 1
        fi
    done
done
printf 'Both preboot matchers passed exact argv and eight rejection cases without tr.\n'
