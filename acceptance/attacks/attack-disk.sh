#!/bin/bash
# Attack A7 — the disk must not give up what the machine knows.
#
# THE CLAIM: "data at rest is protected." Today it is not: the sticks carry their Wi-Fi networks as
# plain keyfiles in /persist/network/system-connections on an **unencrypted** partition, so anyone who
# takes the stick reads the network's password. That is finding F2 in the red-team record.
#
# This script mounts (or reads) the data partition of an image or a real stick and asks the attacker's
# question: is there a credential here, and can I read it? It is expected to FAIL today — a suite whose
# attacks all pass is a suite that is not testing anything.
#
#   images/x86-qemu/attack-disk.sh --dir <path>     # a mounted data partition (used by --self-test too)
#   images/x86-qemu/attack-disk.sh --image <img>    # extract the data partition from an image and read it
#   images/x86-qemu/attack-disk.sh --self-test      # prove the finding can be seen and can be absent
#
# Exit 0 = no readable credential. 1 = a credential was readable (exposed). 2 = could not tell.
set -uo pipefail

# A Wi-Fi keyfile carries `psk=` (WPA) or `password=` inside a [wifi-security] block. Anything that
# matches is a credential an attacker with the stick can read.
exposed() {  # exposed <dir> ; prints and returns
    local d=$1
    [ -d "$d" ] || { echo "UNKNOWN: $d is not a directory"; return 2; }
    local hits
    hits=$(grep -rlE '^[[:space:]]*(psk|password)=[^[:space:]]' "$d" 2>/dev/null | head -5)
    if [ -n "$hits" ]; then
        echo "FAIL: a Wi-Fi credential is readable at rest:"
        printf '%s\n' "$hits" | sed 's/^/      /'
        echo "      (mount the stick on any Linux box and read it — no machine, no password)"
        return 1
    fi
    echo "PASS: no readable credential in $d"
    return 0
}

self_test() {
    local dir fail=0
    dir=$(mktemp -d)
    mkdir -p "$dir/exposed/system-connections" "$dir/clean/system-connections"
    printf '[wifi-security]\npsk=hunter2\n' > "$dir/exposed/system-connections/Home.nmconnection"
    printf '[connection]\nid=Home\n' > "$dir/clean/system-connections/Home.nmconnection"
    expect() { local name=$1 want=$2 path=$3 got; exposed "$path" >/dev/null; got=$?;
        if [ "$got" = "$want" ]; then echo "ok   $name"; else echo "FAIL $name (got $got, want $want)"; fail=1; fi; }
    expect "a readable PSK is caught" 1 "$dir/exposed"
    expect "a keyfile without a secret passes" 0 "$dir/clean"
    expect "a path that is not there is UNKNOWN" 2 "$dir/does-not-exist"
    rm -rf "$dir"
    [ "$fail" = 0 ] && echo "attack-disk: the finding can be seen, and can be absent" || echo "attack-disk: the finding cannot be judged"
    return "$fail"
}

case "${1:-}" in
    --self-test) self_test; exit $? ;;
    --dir) shift; exposed "${1:?usage: --dir <path>}"; exit $? ;;
    --image)
        img=${2:?usage: --image <img>}
        # The data partition's offset and size, straight from the image's own partition table.
        line=$(sfdisk -d "$img" 2>/dev/null | sed -nE 's/.*start= *([0-9]+), size= *([0-9]+),.*name="data".*/\1 \2/p')
        [ -n "$line" ] || { echo "UNKNOWN: no data partition in $img"; exit 2; }
        read -r start size <<< "$line"
        tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
        dd if="$img" of="$tmp/data.img" bs=512 skip="$start" count="$size" status=none || { echo "UNKNOWN: could not extract"; exit 2; }
        if ! mount -o ro,loop "$tmp/data.img" "$tmp/m" 2>/dev/null; then
            # No loop mounts (macOS, or no root): the honest answer is that this check needs Linux.
            echo "UNKNOWN: cannot mount the data partition here — this check runs on the Linux node"
            exit 2
        fi
        exposed "$tmp/m"; exit $?
        ;;
    *) echo "usage: $0 --self-test | --dir <path> | --image <img>"; exit 2 ;;
esac
