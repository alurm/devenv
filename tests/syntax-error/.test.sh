#!/usr/bin/env bash
# Regression test for https://github.com/cachix/devenv/issues/2820
# A syntax error in devenv.nix should produce a useful error message
# (mentioning "syntax error") rather than an unrelated stale warning.

set -uo pipefail

output=$(devenv shell -- true 2>&1)
status=$?

if [ "$status" -eq 0 ]; then
    echo "Test failed: devenv shell should have failed but exited 0"
    echo "Output: $output"
    exit 1
fi

if ! echo "$output" | grep -qi "syntax error"; then
    echo "Test failed: error output does not mention 'syntax error'"
    echo "Output: $output"
    exit 1
fi

if ! echo "$output" | grep -q "devenv\.nix"; then
    echo "Test failed: error output does not reference devenv.nix"
    echo "Output: $output"
    exit 1
fi

# Regression lock for the original symptom of #2820: a stale
# `warning: Ignoring the client-specified setting 'system'…` line was
# shadowing the real syntax error in the headline. The headline (line
# starting with `× Failed`) must never carry that warning text.
plain=$(echo "$output" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g')
headline=$(echo "$plain" | grep "× Failed" | head -n1)
if echo "$headline" | grep -q "Ignoring the client-specified setting"; then
    echo "Test failed: stale warning is shadowing the real error in the headline"
    echo "Headline: $headline"
    exit 1
fi

echo "OK: syntax error correctly reported"
