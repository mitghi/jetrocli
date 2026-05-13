#!/bin/bash
# jetrocli vs jaq, 10 per-row queries over 1 GB NDJSON.
# Output: per-tool wall time + emitted row count, written to /tmp/bench_results.txt.

set -u
FILE=/tmp/big.ndjson
OUT=/tmp/bench_results.txt
> "$OUT"

run() {
    local idx=$1 jq=$2 jeq=$3 desc=$4
    echo "=== Q$idx: $desc ===" | tee -a "$OUT"
    echo "  jetro: $jeq" | tee -a "$OUT"
    echo "  jaq  : $jq"  | tee -a "$OUT"

    local out_j=/tmp/bench_q${idx}_jetro.out
    local out_a=/tmp/bench_q${idx}_jaq.out

    /usr/bin/time -p jetrocli --ndjson -i "$FILE" "$jeq" > "$out_j" 2>/tmp/jc_t
    local jc_real=$(awk '/real/ {print $2}' /tmp/jc_t)
    local jc_rows=$(wc -l < "$out_j")

    /usr/bin/time -p jaq -c "$jq" < "$FILE" > "$out_a" 2>/tmp/ja_t
    local ja_real=$(awk '/real/ {print $2}' /tmp/ja_t)
    local ja_rows=$(wc -l < "$out_a")

    printf "  jetrocli %ss  rows=%s\n" "$jc_real" "$jc_rows" | tee -a "$OUT"
    printf "  jaq      %ss  rows=%s\n" "$ja_real" "$ja_rows" | tee -a "$OUT"
    echo | tee -a "$OUT"
}

run 1  '.id'                                                    '$.id'                                                    "project id"
run 2  '.name'                                                  '$.name'                                                  "project name"
run 3  '.attributes | length'                                   '$.attributes.len()'                                      "attributes count"
run 4  '.attributes | map(.key)'                                '$.attributes.map(@.key)'                                 "attribute keys list"
run 5  '.attributes[0].value'                                   '$.attributes.first().value'                              "first attr value"
run 6  '.attributes[-1].value'                                  '$.attributes.last().value'                               "last attr value"
run 7  '.name | ascii_upcase'                                   '$.name.upper()'                                          "uppercase name"
run 8  '.attributes | map([.key, .value])'                      '$.attributes.map([@.key, @.value])'                      "[key,value] pairs"
run 9  '[.attributes[] | select(.value | contains("_3"))] | length' '$.attributes.filter(@.value.contains("_3")).len()'   "count attrs matching _3"
run 10 'keys'                                                   '$.keys()'                                                "object keys"
