#!/usr/bin/env bash
# scripts/all.sh — every gate in this repository, in one run, with a table at the end.
#
# WHY THIS EXISTS. Measured 2026-11-04: a full sweep found that `m2b`'s demo had stopped running entirely, because a
# later rung (the task abort at M4c) halts the boot before it. The gate was red, but nobody had run it since the abort
# landed, and a gate that cannot reach its subject reports **silence** — which reads like "nothing to report" rather
# than "not measured". The estate's own rule says a suite that cannot reach its subject is not a pass; this script is
# how that rule is enforced rather than remembered.
#
#   scripts/all.sh           # every gate
#   scripts/all.sh --fast    # skip the ones that take longest (currently none; kept for the harness)
#
# Exit code 0 only if every gate is green. A red gate prints its own reason above the table.
set -uo pipefail
cd "$(dirname "$0")/.."

GATES=(m0 m1 m2 m2b mmu task isolation clock tick sched preempt prio cert cap box fdt ab net loader)
pass=0
fail=0
declare -a RED

echo "== every gate"
for gate in "${GATES[@]}"; do
    if [ ! -x "scripts/$gate.sh" ] && [ ! -f "scripts/$gate.sh" ]; then
        printf "%-10s %s\n" "$gate" "MISSING — the script does not exist, which is not a pass"
        RED+=("$gate")
        fail=$((fail + 1))
        continue
    fi
    out=$(bash "scripts/$gate.sh" 2>&1)
    rc=$?
    last=$(printf '%s\n' "$out" | grep -E '^(ok|FAIL)' | tail -1)
    if [ $rc -e0 ] 2>/dev/null; then :; fi
    if [ "$rc" -eq 0 ] && [ "${last#ok}" != "$last" ]; then
        printf "%-10s %s\n" "$gate" "green"
        pass=$((pass + 1))
    else
        printf "%-10s %s\n" "$gate" "RED — ${last:-no verdict}"
        RED+=("$gate")
        fail=$((fail + 1))
    fi
done

echo
echo "== summary: $pass green, $fail red"
if [ "$fail" -ne 0 ]; then
    echo "red: ${RED[*]}"
    exit 1
fi
echo "every gate reached its subject and passed"
