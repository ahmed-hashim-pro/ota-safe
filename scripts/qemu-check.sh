#!/bin/sh
# Runs the ota-safe bootloader, built for a Cortex-M3, in QEMU on every
# depth-1 power-cut state, and checks it decides exactly what the host
# simulator decided. Needs: cargo with thumbv7m-none-eabi, qemu-system-arm.
set -eu
SIM=${SIM:-target/release/ota-sim}
ELF=qemu/target/thumbv7m-none-eabi/release/ota-safe-qemu
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT

key=$(sed -n '/PUBLIC_KEY: \[u8; 32\] = \[/,/\];/p' qemu/src/main.rs | grep -o '0x[0-9a-f][0-9a-f]' | sed 's/0x//' | tr -d '\n')
[ "$($SIM key)" = "$key" ] || {
    echo "qemu/src/main.rs PUBLIC_KEY does not match ota-sim key" >&2
    exit 1
}
$SIM states --dir "$DIR"
n=0
bad=0
while IFS="$(printf '\t')" read -r file want; do
    got=$(qemu-system-arm -machine mps2-an385 -nographic -monitor none \
        -semihosting-config enable=on,target=native -kernel "$ELF" \
        -device loader,file="$DIR/$file",addr=0x20300000)
    n=$((n + 1))
    if [ "$got" != "$want" ]; then
        bad=$((bad + 1))
        printf 'MISMATCH %s\n  host: %s\n  qemu: %s\n' "$file" "$want" "$got"
    fi
done < "$DIR/expected.txt"
echo "$n flash images: the Cortex-M3 build decided as the host did on $((n - bad)) of $n"
[ "$bad" -eq 0 ]
