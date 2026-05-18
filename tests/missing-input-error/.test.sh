#!/usr/bin/env bash
# Regression test for https://github.com/cachix/devenv/issues/2820 follow-ups.
# When `devenv.yaml` is missing an input that `devenv.nix` references (here
# `git-hooks`), the actionable "devenv inputs add ..." suggestion should
# appear right after the error headline, not be buried under ~100 lines of
# Nix `--show-trace` evaluation frames.

set -uo pipefail

output=$(devenv shell -- true 2>&1)
status=$?

if [ "$status" -eq 0 ]; then
    echo "Test failed: devenv shell should have failed but exited 0"
    echo "Output: $output"
    exit 1
fi

# Strip ANSI escapes so the line-position check isn't fooled by colors.
plain=$(echo "$output" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g')

# Find the line number of the diagnostic headline ("× Failed to ..."). The
# actionable suggestion must follow within a handful of lines, before the
# `help:` section that carries the `--show-trace` body.
headline=$(echo "$plain" | grep -n "Failed to get shell attribute" | head -n1 | cut -d: -f1)
if [ -z "$headline" ]; then
    echo "Test failed: did not find 'Failed to get shell attribute' headline"
    echo "Output: $output"
    exit 1
fi

window=$(echo "$plain" | sed -n "${headline},$((headline + 5))p")
if ! echo "$window" | grep -q "devenv inputs add git-hooks"; then
    echo "Test failed: 'devenv inputs add git-hooks' suggestion was not in the 5 lines following the headline"
    echo "Window (lines ${headline}..$((headline + 5))):"
    echo "$window"
    exit 1
fi

# Regression lock for the warning-shadow symptom of #2820 in the
# missing-input scenario (the second qaristote follow-up). The headline must
# not carry the stale `Ignoring the client-specified setting 'system'` warning.
headline_line=$(echo "$plain" | sed -n "${headline}p")
if echo "$headline_line" | grep -q "Ignoring the client-specified setting"; then
    echo "Test failed: stale warning is shadowing the real error in the headline"
    echo "Headline: $headline_line"
    exit 1
fi

echo "OK: missing-input suggestion appears next to the error headline"
