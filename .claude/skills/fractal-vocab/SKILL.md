---
name: fractal-vocab
description: 監督が残した語彙の要望（運動、形状、マテリアル、光）を検討し、採用するものを実装してテストと見本で裏付ける。制作の外で行う開発作業。
argument-hint: "[要望番号 ...]"
disable-model-invocation: true
---

# 語彙の開発

監督エージェントは、撮りたい場面を語彙で表せなかったときに要望を残す。あなたの仕事は、その要望を検討し、採用するものを実装して、次の制作から使えるようにすることです。対象は `$ARGUMENTS` の番号の要望で、指定がなければ未対応の要望すべてです。

制作中の作品には触れない。`productions/` 以下は読むだけにする。

## 1. 要望を読んで仕分ける

```bash
cargo build --release -p fractal-renderer-harness
./target/release/fractal-harness vocabulary list --open
```

要望ごとに、次のどれかに決める。

- **見送り（すでに表せる）**: いまの語彙で同じ場面が撮れる。例：「寄って終わる」は `dive` がある。「深部から全体へ引いていく」は `flythrough`（`direction: out`）がある。どう書けば撮れるかを note に書く。寄る前の静止は意図的に語彙から外してあるので、この要望は「範囲外」にする。
- **見送り（範囲外）**: 型付き DSL の方針（任意の WGSL を受け付けない、検証できない入力を作らない）に反する、または1つの作品でしか使えない。
- **採用**: 複数の作品で使える一般的な表現で、検証とテストを書ける。似た要望はまとめて1つの機能にする。

判断の基準は「監督が場面の言葉で書いたことを、ほかの形状やほかの作品でも撮れるようになるか」。1つの制作の事情に合わせた特例は作らない。

## 2. ブランチを切る

```bash
git switch -c vocab/<短い名前>
```

main には直接コミットしない。マージは人が行う。

## 3. 種類ごとの実装箇所

どの種類でも、仕様書 `docs/harness-spec.md` と、既存の同種の実装を先に読むこと。値の範囲は必ず検証し、範囲外の入力はエラーにする。

### 運動（motion）

カメラ経路がレンダラーにあるものの組み合わせで足りる場合は、ハーネスだけで済む。

- `renderer-harness/src/cut.rs`: `MotionSpec` に変種を足す。`motion()`、`typical()`、`validate()`（範囲）、`compile()`（経路への変換）を揃える。
- `renderer-harness/src/axes.rs`: 多様性の軸として区別したい運動なら `Motion` に値を足し、`renderer-harness/src/classify.rs` の `motion()` を合わせる。
- `renderer-harness/src/brief.rs` の `motion_feasible()`: どの形状でも計画できる運動なら、常に実現可能な側に加える。
- `renderer-harness/src/schema.rs`: `shotlist` の見本に、書き方と範囲を1件足す（見本の例がすべて検証を通ることをテストが確かめる）。
- 新しい経路が要る場合は `renderer-core/src/path.rs`、`animation.rs`、`scene_file.rs` に、検証・計画・サンプリング・YAML の読み書きを揃えて足す。

### 形状（shape）

- `renderer-core/src/dsl.rs`: `OrbitTransform` に変種、`validate()` に範囲、`generate_wgsl()` に固定テンプレートでの生成を足す。距離推定の導関数の更新を忘れない。
- `renderer-core/src/fractal.rs`: CPU 側の `DistanceEstimator` に同じ変換を足す。GPU と CPU で同じ式にすること（経路検査と視点探索は CPU 側を使う）。
- `renderer-core/src/scene_file.rs`: YAML との相互変換。
- `renderer-harness/src/axes.rs`: `FormulaFamily` に族を足し、`classify_family()` で判定できるようにする。
- `renderer-harness/src/shape.rs`: `generate()` に族のテンプレート（係数の範囲）を、`mutate()` に近傍探索を足す。

### マテリアル（material）・光（light）

- `renderer-core/src/dsl.rs` の `DslMaterial`（または `config.rs` の `LightConfig`）、WGSL 生成、`scene_file.rs`。
- `renderer-harness/src/look.rs`: 監督が書く `MaterialSpec` などに項目を足し、`grade()` で変換する。スケールに依存する値は被写体距離に比例させる。
- `renderer-harness/src/schema.rs`: `look` の見本に項目と範囲を足す。
- 光の分類を変えるなら `renderer-harness/src/classify.rs` と `axes.rs`。

## 4. 受け入れの条件

次をすべて満たすこと。満たせなければ、その要望は見送りにして理由を書く。

1. **テスト**: 追加した検証の範囲、変換の結果、見本の例が通ること。既存の同種のテスト（`cut.rs`、`shape.rs`、`look.rs`、`schema.rs`）にならう。
   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
2. **形状族の見本**: 新しい族は、ロケハンにかけたときの歩留まりを測る。
   ```bash
   ./target/release/fractal-harness vocabulary sample --family <族> --formulas 30 --out output/vocab/<族>
   ```
   - 返り値の `shortfalls` が空であること（使える形状の割合と、選別を通る視点の割合を、調整用設定 `vocabulary` の下限と照らした結果）。
   - `sheets` の画像を自分で開いて見て、既存の族と見分けがつく形が出ていること。似た絵しか出ないなら、族として足す意味がない。
   - `median_frame_seconds` が既存の族と同程度であること（既存の族でも同じコマンドで測って比べる）。
3. **運動・マテリアル・光の見本**: 既存の制作に手を加えず、試験用の制作で確かめる。
   ```bash
   ./target/release/fractal-harness --root output/vocab/productions init <名前> --seed 1
   ```
   ロケハン、ロケ地の選択、追加した表現を使ったブリーフ、`brief check`、`storyboard` まで進め、シートを見て意図どおりの絵になっていることを確かめる。マテリアルと光は `look preview` まで進める。
4. **仕様書**: `docs/harness-spec.md` の該当箇所（運動の表、軸の値、`look.yaml` の説明など）を更新する。
5. **調整値**: 新しく入れた閾値や上限は、コードに直接書かず `renderer-harness/src/tuning.rs` に置く。

GPU を使うコマンドは、環境変数 `WGPU_BACKEND` がこの機械に合っていること（WSL では `gl`）。

## 5. 記録して終える

採用した要望も見送った要望も、1件ずつ結果を記録する。

```bash
./target/release/fractal-harness vocabulary resolve <番号> --status implemented \
  --note "運動 crane: { kind: crane, rise: 0.5 }。ブランチ vocab/crane"
./target/release/fractal-harness vocabulary resolve <番号> --status declined \
  --note "orbit を俯瞰の視点から使えば同じ場面が撮れる"
```

実装した分はブランチにコミットする。最後に、次を短く報告して終える。

- 採用した要望と、追加した語彙の書き方。
- 見送った要望と、その理由。
- 見本の結果（歩留まり、コスト、シートの場所）。
- マージ前に人が見るべき点。
