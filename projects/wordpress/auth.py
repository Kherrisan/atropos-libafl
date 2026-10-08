#!/usr/bin/env python3
"""Run after php-cgi is listening. WordPress fuzzing does not require a login cookie."""

import json
import os

destination = "/var/lib/atropos/auth.json"
os.makedirs(os.path.dirname(destination), exist_ok=True)
with open(destination, "w", encoding="utf-8") as handle:
    json.dump({"cookies": {}, "fields": {}}, handle)
    handle.write("\n")
