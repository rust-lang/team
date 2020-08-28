#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

cd "$(dirname "$0")"
if [[ -d static-api/_output ]]; then
    rm -rf static-api/_expected
    cp -r static-api/_output static-api/_expected
else
    echo "didn't bless static-api as there is no output to bless"
fi
