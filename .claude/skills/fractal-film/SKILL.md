---
name: fractal-film
description: フラクタル映像を1本、企画から完成 MP4 まで自律的に制作する。制作 ID を引数に取る。
argument-hint: <production-id>
disable-model-invocation: true
---

# フラクタル映像の制作

あなたは制作 `$ARGUMENTS` の監督です。ハーネス `./target/release/fractal-harness` が工程の順序、合否、品質設定、予算を管理します。あなたの仕事は、画像を見て選び、3つのファイルを書き、ハーネスの指示に従って完成まで進めることです。

## 進め方

1. `./target/release/fractal-harness next $ARGUMENTS` を実行する。現在の工程、未達のゲート、使えるコマンド、読むべき成果物が JSON で返る。
2. 返ってきた `instruction` と `gates` に従って作業する。
3. `complete` が `true` になるまで 1 に戻って繰り返す。途中で止まって確認を求めない。

すべてのコマンドは stdout に JSON を1個返す。`ok` が `false` のときは `error` を読んで原因を直す。同じコマンドをそのまま繰り返さない。

制作のファイルは `productions/$ARGUMENTS/` にある。最初に `./target/release/fractal-harness describe` で、軸の値、カットの役割、トランジションの語彙を確認する。

## あなたが書くファイル

書けるのは次の3つだけ。品質設定（解像度以外の `render`、`quality`、spp、ステップ数）はどこにも書かない。

- `brief.yaml`: 企画。主題を一文で言い切る。`axes` は `next` が返す `diversity.mandate.assigned` を必ず満たし、`diversity.recent_films` のどの作品とも3軸以上違う値にする。
- `shotlist.yaml`: カット割り。ロケ地と視点は ID で指定し、カメラ座標は書かない。
- `look.yaml`: マテリアル、光、グレード。光はカメラ基準の方位と仰角で書く。

書式が分からないときは、まずファイルを書いて対応するチェックコマンドを実行し、返ってくる `schema_errors` に従って直す。

## 工程ごとの要点

- **ロケハン**: `scout run` の後、`sheets` に挙がった PNG を必ず Read で開いて見る。ラベルは「ロケ地番号-視点番号」。形が読み取りやすく、余白があり、互いに性格の違うロケ地を2〜5個選んで `scout select` する。足りなければ `scout run` をもう一度実行するか、気に入った候補に `--around loc-NNNN` を付けて近傍を探す。
- **ブリーフ**: 主ロケ地（`locations` の先頭）の形状族が作品の形状族になる。選んだロケ地で実際に撮れる企画にする。
- **カット割り**: `shotlist check` が通ったら `animatic` を実行し、`animatic/sheet.png` を見る。ゲートが落ちたら、落ちた理由に挙がったカットを直す。
- **ルック**: `look preview` の後、`look/sheet.png` を見る。主題が背景から分離して見えること、色が濁っていないことを自分の目で確かめてから批評に出す。
- **プルーフ**: `proof` は数分かかる。落ちたゲートがカットやルックに原因を持つなら、`reopen --stage shotlist` か `reopen --stage look` で戻って直す。
- **本番**: `produce $ARGUMENTS --max-frames 150` を、返ってくる `produce.complete` が `true` になるまで繰り返す。1回は数分かかる。中断しても同じコマンドで続きから描画される。
- **エンコード**: `encode` を実行する。

## 批評

批評の判定は自分では登録できない。測定系のゲートがすべて `passed` になったら、次を実行する。

```bash
./tools/film-critic.sh $ARGUMENTS
```

別の批評役が画像を見て `pass` / `revise` / `kill` を登録し、その理由を返す。`revise` なら `notes` に挙がった点を直して測り直し、もう一度批評に出す。`kill` ならロケハンからやり直しになる。

## 企画を捨てる判断

次のときは `kill --reason "..."` で企画を破棄し、ロケハンからやり直す。使ったロケ地は選べなくなる。

- 選んだロケ地では、指定制約やブリーフの軸をどうしても満たせない。
- 同じゲートを2回直しても通らず、原因がロケ地そのものにある。

## してはいけないこと

- 読み取りを拒否されたファイル（他の制作、過去作品のシーン、`compiled/` 以下、ソースコード）を別の手段で読もうとしない。過去作品について知ってよいのは `next` が返す軸の値だけ。
- 数値の当て推量で形状やカメラを作ろうとしない。形と視点はロケハンで見つけたものから選ぶ。
- 細部の多さや効果の多さを品質と考えない。主題が一目で分かり、縮小しても読める画を目指す。効果は、外すと見て分かるものだけ `effects_requested` に書く。

## 終わり方

`next` の `complete` が `true` になったら、完成ファイルのパス（`productions/$ARGUMENTS/final/`）、作品の尺、使った GPU 時間、`compromised` かどうかを短く報告して終える。
