#!/usr/bin/env bash
# Reviews the current stage of a production with a critic that runs as its
# own Claude process, then records the verdict.
#
# The critic sees the brief's subject and the rendered images only. It never
# sees the shot list, the look, or the director's reasoning, so it cannot
# simply confirm what the director intended.
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: tools/film-critic.sh <production-id>" >&2
    exit 2
fi
id=$1
harness=./target/release/fractal-harness
root=${FRACTAL_HARNESS_ROOT:-productions}
ledger=${FRACTAL_HARNESS_LEDGER:-portfolio/ledger.jsonl}
production=$root/$id

# The stage decides what there is to look at and what to ask about it.
stage=$("$harness" --root "$root" --ledger "$ledger" next "$id" |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["stage"])')
case "$stage" in
shotlist)
    materials=("$production/animatic/sheet.png" "$production/animatic/report.json")
    rubric='1. 各カットの主題が一目で分かり、下の「主題」と一致するか。
2. 縮小されたこの大きさでも、主題と背景が区別できるか。
3. 過去作品のキーフレームと比べて「角度違い・寄り違い」に見えないか。
4. カット同士が、スケール、動き、構図のいずれかで明確に違うか。
5. 始点・中間・終点の変化から見て、動きが速すぎたり、止まって見えたりしないか。

この工程の画像は、形と動きだけを見るために中立の灰色で描かれている。色と光は次の工程で判定するので、色がないこと、逆光や配色が確認できないことは不合格の理由にしない。'
    ;;
look)
    materials=("$production/look/sheet.png" "$production/look/report.json")
    rubric='1. 各カットの主題が一目で分かり、下の「主題」と一致するか。
2. 主題が背景から色と明るさで分離しているか。濁りや白飛び、黒潰れはないか。
3. 過去作品のキーフレームと比べて「色違い」に見えないか。
4. 色数が絞られ、作品全体でルックが一貫しているか。'
    ;;
proof)
    materials=("$production/proof/sheet.png" "$production/proof/report.json")
    rubric='1. 各カットの主題が一目で分かり、下の「主題」と一致するか。
2. 配信用に圧縮されたこの画像で、細部が潰れてノイズや塗りつぶしになっていないか。
3. 過去作品のキーフレームと比べて焼き直しに見えないか。
4. 粒状のノイズや縞が目立たないか。'
    ;;
*)
    echo "{\"ok\":false,\"error\":\"stage $stage has no critic review\"}"
    exit 1
    ;;
esac
for material in "${materials[@]}"; do
    if [[ ! -f $material ]]; then
        echo "{\"ok\":false,\"error\":\"$material is missing; run the stage's measuring command first\"}"
        exit 1
    fi
done

subject=$(python3 - "$production/brief.yaml" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
for key in ("title", "subject", "arc"):
    match = re.search(rf"^{key}:\s*(.+)$", text, re.M)
    print(f"{key}: {match.group(1).strip() if match else '(none)'}")
PY
)
# Past films are shown as one sheet of their keyframes, so the amount the
# critic has to look at does not grow with the portfolio.
portfolio_sheet=$production/reviews/portfolio.png
portfolio=$("$harness" --root "$root" --ledger "$ledger" ledger sheet "$portfolio_sheet" |
    python3 -c '
import json, sys
films = json.load(sys.stdin).get("films") or []
for film in films:
    print("- 番号 {number}: {title}".format(**film))
')

prompt=$(
    cat tools/critic-prompt.md
    printf '\n## 主題\n\n%s\n' "$subject"
    printf '\n## 材料\n\n'
    printf -- '- %s\n' "${materials[@]}"
    printf '\n## 過去作品のキーフレーム\n\n'
    if [[ -z $portfolio ]]; then
        printf '（まだ作品はありません。項目3は合格としてください）\n'
    else
        printf '次のシートを Read で開く: %s\nラベルは「作品番号-キーフレーム番号」です。\n%s\n' \
            "$portfolio_sheet" "$portfolio"
    fi
    printf '\n## ルーブリック（工程: %s）\n\n%s\n' "$stage" "$rubric"
)

# The critic may read exactly the review materials and past keyframes.
settings=$(python3 - "$production" <<'PY'
import json, sys
production = sys.argv[1]
print(json.dumps({"permissions": {
    "allow": [
        f"Read(./{production}/animatic/**)",
        f"Read(./{production}/look/**)",
        f"Read(./{production}/proof/**)",
        f"Read(./{production}/reviews/portfolio.png)",
    ],
    "deny": [
        f"Read(./{production}/compiled/**)",
        f"Read(./{production}/shotlist.yaml)",
        f"Read(./{production}/look.yaml)",
        f"Read(./{production}/scout/**)",
        "Read(./scenes/**)",
        "Read(./output/**)",
        "Read(./portfolio/**)",
    ],
}}))
PY
)

response=$(claude -p "$prompt" \
    --settings "$settings" \
    --tools "Read" \
    --permission-mode dontAsk \
    --max-turns 40 \
    ${FILM_CRITIC_MODEL:+--model "$FILM_CRITIC_MODEL"} \
    --output-format json)

# The critic answers with one JSON object; anything else is not a verdict.
read -r verdict notes < <(python3 - "$response" <<'PY'
import base64, json, re, sys
result = json.loads(sys.argv[1]).get("result", "")
objects = re.findall(r"\{[^{}]*\"verdict\"[^{}]*\}", result, re.S)
if not objects:
    sys.exit("the critic did not return a verdict: " + result[:400])
answer = json.loads(objects[-1])
verdict = answer.get("verdict")
if verdict not in ("pass", "revise", "kill"):
    sys.exit(f"the critic returned an unknown verdict {verdict!r}")
notes = str(answer.get("notes", "")).strip() or "(no notes)"
failed = answer.get("failed") or []
if failed:
    notes = f"failed items {failed}: {notes}"
# Encoded so that any characters in the notes survive the shell.
print(verdict, base64.b64encode(notes.encode()).decode())
PY
) || true
if [[ -z ${verdict:-} ]]; then
    echo '{"ok":false,"error":"the critic did not return a usable verdict; run the review again"}'
    exit 1
fi
notes=$(printf '%s' "$notes" | base64 -d)

FRACTAL_HARNESS_ROLE=critic "$harness" --root "$root" --ledger "$ledger" \
    review submit "$id" --verdict "$verdict" --notes "$notes"
