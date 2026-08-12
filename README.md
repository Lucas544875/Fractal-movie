# Fractal Movie

Rust、wgpu、WGSL で3次元フラクタルを描画する、ウィンドウ不要のオフスクリーンレンダラーです。fullscreen triangle の fragment shader から Mandelbulb または Mandelbox の Distance Estimator を ray marching し、PNG へ保存できます。

## 現在の実装範囲

- wgpu による headless adapter/device 初期化（画面・surface 不要）
- WGSL による Mandelbulb / Mandelbox、sphere tracing、法線、フラクタル別ライティング
- CPU Distance Estimator による再利用可能なカメラターゲット探索
- CPU/WGSL共通の4×`f32` quad-float座標と高精度Mandelbox DE
- 十進文字列または4 limb展開を保持できる高精度scene camera
- overview から深部へ進む指数ズーム経路モデル
- scene定義のFPS/frame countによるquad-float animationとフレーム単位の動的DE調整
- `frame_%06d.png`連番、検証付きresume、任意フレーム出力、明示的overwrite
- scene/CLIで設定できるFFmpeg動画encodeと原子的な動画出力
- HDR sample accumulation、AO、面積を持つdirectional lightのsoft shadow、1 bounce reflection
- 薄レンズcameraによる被写界深度・円形ボケとextended Reinhard tone mapping
- 型付きfractal DSL/AST、限定orbit命令、CPU validation、WGSL code generation
- `Rgba8UnormSrgb` オフスクリーン texture から row alignment を考慮した readback
- PNG 出力を GPU renderer から分離
- 解像度、ray steps、fractal iterations、NaN/Inf などの事前 validation
- WGSL validation error を文脈付きエラーとして報告
- GPU 名、解像度、フレーム時間、合計時間のログ
- content-addressed scene revisionとtransactional patch/promotion
- JSONL agent harness、tool schema discovery、idempotency key、structured error
- persisted asynchronous preview/render/encode jobとresource budget
- preview metrics/contact sheet、DE target候補、camera route clearance検査
- scene hash付きframe sequence、complete/range/available partial encode

Phase 6まで実装済みで、型付きDSLから生成したフラクタルにもoffline品質の光学・ライティング効果、PNG連番、FFmpeg動画encodeを適用できます。

## 必要環境

- Rust 1.87 以上（`rust-toolchain.toml` は検証済みの 1.97.1 を指定）
- Vulkan / Direct3D 12 / Metal / OpenGL ES のいずれかを利用できる wgpu 対応 driver
- PNG 1枚の出力には FFmpeg は不要
- `video`を有効にした動画変換にはFFmpeg（実行ファイルは`--ffmpeg`で指定可能）

Linux の headless 環境では Vulkan loader と対象 GPU の Vulkan driver が必要です。レンダラーはハードウェア GPU を既定で必須とし、llvmpipe/lavapipe などの software adapter へ暗黙にフォールバックしません。選択結果は起動時の `Adapter:` と `Acceleration:` で確認できます。

## Build と検証

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## GPU の診断

wgpu から見える全アダプターと、ハードウェアアクセラレーションの状態を表示します。

```bash
cargo run -p fractal-renderer-cli -- gpu-info
```

特定のアダプターを選ぶ場合は名前の一部を指定できます。

```bash
cargo run --release -p fractal-renderer-cli -- render --adapter NVIDIA
```

`WGPU_BACKEND` も利用できます。特に WSL でVulkanドライバーがGPUを公開せず、MesaのD3D12 OpenGL経路が利用可能な場合は、次の診断結果を比較してください。

```bash
WGPU_BACKEND=vulkan cargo run -p fractal-renderer-cli -- gpu-info
WGPU_BACKEND=gl cargo run -p fractal-renderer-cli -- gpu-info
```

WSLではWindows側の対応GPUドライバー、WSL2、`/dev/dxg` が必要です。`nvidia-smi` が失敗する、または `/dev/dxg` が存在しない場合はRustアプリより下層でGPUが公開されていません。Windows側でGPUドライバーとWSLを更新し、`wsl --shutdown` 後に再起動してください。

動作確認のためCPUレンダリングを意図的に許可する場合だけ `--allow-software` を指定できます。この場合はアクセラレーションされません。

```bash
cargo run -p fractal-renderer-cli -- render --allow-software
```

### GPUデューティ比の制限

対話作業中にoffline renderがGPUを占有し続けないよう、`--gpu-duty-cycle`でこのrendererの平均GPU稼働時間を1〜100%に制限できます。たとえば40%では、各GPU stripに100 msかかった場合、その完了後に150 ms休止します。制限中はstripも短く分割されるため、ほかのGPU clientへ定期的に実行機会を返します。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --gpu-duty-cycle 40
```

これはstrip単位の平均デューティ比であり、処理中の瞬間的なhardware utilizationや、ほかのprocessを含むGPU全体の使用率を保証するものではありません。未指定時は休止を入れず従来どおり最大速度で描画し、`100`も休止なしになります。PNGと動画の画質・決定性は変わらず、設定値に応じて所要時間だけが増えます。

## 作業用プレビュー

`preview`はsceneをメモリ上で一時的に軽量化し、元のYAMLを書き換えずに構図、軌道、色、照明、被写界深度を確認します。animationでframeを指定しなければ、始点、1/4、1/2、3/4、終点の代表5 frameを自動選択します。

```bash
cargo run --release -p fractal-renderer-cli -- preview \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --profile composition
```

各profileは次の用途を想定しています。

| profile | 既定の最大幅 | sampling / effect | 用途 |
|---|---:|---|---|
| `composition` | 320 px | 1 spp、DOF・AO・shadow・reflectionなし | camera、target、軌道、遮蔽 |
| `lookdev` | 480 px | 最大8 spp、軽量AO・shadow、DOFなし | palette、material、light、tone mapping |
| `proof` | 810 px | 最大32 spp、DOFと中品質の二次効果 | ボケ、highlight、最終印象 |
| `final` | sceneの解像度 | sceneの設定をすべて維持 | 本番品質の部分確認 |

任意frameは単数またはカンマ区切りで指定できます。出力先の既定値は`output/preview/<scene>/<profile>/`です。

```bash
# 代表位置を指定してlook development
cargo run --release -p fractal-renderer-cli -- preview \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --profile lookdev --frames 0,180,360,540

# sceneのaspect ratioを保ったまま幅だけoverride
cargo run --release -p fractal-renderer-cli -- preview \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --profile proof --frame 180 --width 640
```

`--watch`は250 ms間隔でscene内容を監視し、保存後に同じframeを再描画します。camera、light、render parameterなどuniformだけの変更ではGPU pipelineとallocationを再利用します。fractal DSLのgeometryやmaterialはshader定数なので、その変更時だけpipelineを再構築します。編集中に一時的にYAMLが不正になっても監視は終了せず、次の保存を待ちます。

```bash
cargo run --release -p fractal-renderer-cli -- preview \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --profile lookdev --frame 180 --watch --gpu-duty-cycle 60
```

`--region x,y,width,height`は左上を原点とする0〜1の正規化座標です。カメラを寄せるのではなく、完全なviewportの投影と画素密度を維持したまま指定範囲だけをGPUで描画します。下の例は本番解像度・本番品質で中央50%だけを確認し、計算対象を約1/4にします。cropは既定ではprofile directory内の`crop/`へ保存されるため、全画面previewを上書きしません。

```bash
cargo run --release -p fractal-renderer-cli -- preview \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --profile final --frame 180 --region 0.25,0.25,0.5,0.5
```

## Agent harness

自律エージェント向けには、scene YAMLとCLIログを直接操作する代わりに、immutable revision、非同期job、artifact、画像metricsを扱う`fractal-harness`を利用できます。protocolはstdin/stdout上のnewline-delimited JSONで、`capabilities.describe`からtool schemaを取得できます。

```bash
cargo run --release -p fractal-renderer-harness -- \
  --root output/harness --max-gpu-duty-cycle 80
```

Alchemyをprojectへ取り込み、軌道revisionを作成し、代表previewを比較して、80%制限のfinal renderとpartial/complete encodeへ進むtool-call例は[`docs/agent-harness.md`](docs/agent-harness.md)にあります。P0は実プロセスJSONL契約、再起動復旧、panicのterminal化、atomic artifact公開を含めて完了しています。設計境界、検証コマンド、後から追加すると破壊的変更になる推奨機能の優先順位は[`docs/agent-harness-roadmap.md`](docs/agent-harness-roadmap.md)に固定しています。

## Mandelbulb を1フレーム出力

デフォルトは 640x360 の画像を `output/phase1/mandelbulb.png` に保存します。

```bash
cargo run --release -p fractal-renderer-cli -- render
```

出力先と解像度は変更できます。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  --output output/phase1/mandelbulb-1080p.png \
  --width 1920 \
  --height 1080
```

## Portfolio Mandelbox を1フレーム出力

`../portfolio/site/works/mandelbox/` と同じ box fold / sphere fold パラメーター、茶色の材質、6方向の橙色発光を使います。CPU側では参照JSの `pickOriginGapDir()` と同じ方針で、+X側から実際に到達できる表面を探索してカメラを配置します。

```bash
cargo run --release -p fractal-renderer-cli -- render --fractal mandelbox
```

出力先は既定で `output/phase1/mandelbox.png` です。ターゲット探索はseedに対して決定的なので、同じseedなら同じ構図になります。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  --fractal mandelbox \
  --seed 20260811 \
  --output output/phase1/mandelbox-alt.png
```

経路関連のコードはGPUレンダラーから独立しています。`DistanceEstimator`を実装したフラクタルは`TargetPicker`による表面ターゲット探索を再利用でき、`ExponentialDivePath::distance_at()`は参照JSのoverview→diveの距離曲線を提供します。この探索は後述の`multi-target-dive`と`surface-flyover`からscene経由で利用できます。

## Scene file（Phase 2・実装済み）

`version: 1` の YAML から camera、fractal、light、render、seed を読み込み、GPU 初期化前に検証します。Mandelbulb と Portfolio Mandelbox のサンプルは `scenes/examples/` にあります。

```bash
cargo run --release -p fractal-renderer-cli -- \
  render scenes/examples/mandelbulb.yaml
```

主要フィールドは `fractal.kind/parameters`、`camera.position/target/up/vertical_fov_degrees/aperture_radius/focus_distance`、`light.direction`、`render.width/height/max_steps/max_distance/epsilon/step_safety/pixel_epsilon_multiplier`、`quality`、`animation`、`video`、`seed`、`precision` です。未知の version・フィールドや範囲外の値はエラーにし、自由な WGSL 全体は scene に埋め込みません。

scene の値は、明示した CLI オプションだけで上書きできます。

```bash
cargo run --release -p fractal-renderer-cli -- \
  render scenes/examples/mandelbox.yaml \
  --width 1920 --height 1080 --seed 20260811 \
  --output output/mandelbox-1080p.png
```

`--output` を省略した静止画sceneの出力先は `output/<scene名>/<scene名>.png` です。既存ファイルを置換するときは`--overwrite`を明示します。`precision: quad-float` はMandelboxで利用できます。現在Mandelbulbをquad-floatへ変更すると、未対応であることを明示したエラーになります。

## Quad-float 高精度座標（Phase 2.5・実装済み）

4個の`f32`を大きい順の非重複な展開として保持する`Qf32`と`QfVec3`をCPU/WGSLへ実装しています。汎用IEEE 754任意精度floatではなく、Mandelboxの超高倍率ズームに必要な演算へ対象を限定した約90 bitの仮数精度です。演算は[Hida–Li–Baileyのquad-doubleアルゴリズム](https://escholarship.org/uc/item/69q5t2mj)と[Shewchukの浮動小数点展開](https://people.eecs.berkeley.edu/~jrs/papers/robustr.pdf)を基礎にしています。

サンプルの1e-14 camera distanceは、絶対座標を`f32`へ変換するとcameraとtargetが同一値へ丸められますが、quad-float経路では分離したまま描画できます。

```bash
cargo run --release -p fractal-renderer-cli -- \
  render scenes/examples/mandelbox-quad-deep.yaml
```

組み込みpresetから任意の単一ズーム画像を生成する場合は、camera distanceを十進文字列として直接指定できます。軸上の解析的な外側境界を基準点とし、shader内ではその境界からの相対座標でrayとDEを評価します。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  --fractal mandelbox \
  --precision quad-float \
  --camera-distance 1e-14 \
  --width 640 --height 360
```

sceneのcamera座標は通常の数値に加え、`"2.2800000000000000000000000001"`のような仮数部最大35桁の十進文字列と、`[high, low1, low2, low3]`形式を受け付けます。十進文字列は`f64`を経由せず`Qf32`へ変換され、`LoadedScene::to_yaml()`は4 limbを損失なく出力します。

### 精度境界

- quad-float対象: camera position/target、境界相対のray位置、Mandelbox反復座標、sphere foldの半径と係数
- 深部DEは`x = 2 × fold_limit`を原点とする相対座標を使い、`O(1)`の絶対値へ`1e-N`のray offsetを再加算しない
- DE反復数はcamera distanceから`ceil(log(1/distance) / log(abs(scale))) + 5`で算出し、16〜96回へ制限
- 組み込みquad-float Mandelboxは128 ray stepで全画素の収束を確認。sceneで指定できる安全上限は1024回
- `f32`維持: rayの正規化済み方向、travel、色、時間、ライト、法線の最終ベクトル
- `PathTarget.point`は`QfVec3`になり、一般経路は`TargetPicker::refine()`で高精度化、組み込みpresetは解析的境界を使用
- CPU演算は160 bit MPFR oracleに対して加算`1e-24`、乗算`1e-23`、除算`1e-22`の相対誤差基準でテスト
- exponent範囲は`f32`と同じで、任意精度型ではない

### GPU実測

RTX 3070をMesa D3D12/OpenGL経路で使用した320x180、同一overview sceneの単一フレーム結果です。

| precision | frame time | f32比 |
|---|---:|---:|
| `f32` | 0.261秒 | 1.00× |
| `quad-float` | 0.470秒 | 1.80× |

overview画像同士のnormalized RMSEは`2.11e-5`でした。深部は同じ環境の320x180でcamera distance `1e-14`と`1e-26`を描画し、全画素の表面ヒットと空間的な色変化を確認しています。`1e-27`では有効な表面画素を得られなかったため、組み込みpresetの保証下限は安全側の`1e-26`とし、それ未満はエラーにします。この境界はGPU・backend・compilerで変化します。

実装配置は次のとおりです。

```text
renderer-core/
├── precision/
│   ├── quad_float.wgsl
│   └── quad_float_vec3.wgsl
└── src/precision/
    ├── mod.rs
    ├── quad_float.rs
    └── coordinate.rs
```

## Animation と任意フレーム（Phase 3・実装済み）

`animation.fps`と`animation.frame_count`から`time = frame / fps`を決定し、`ExponentialDivePath`でoverview保持後に指数ズームします。サンプルは60 fps・1621フレームで、0〜4秒を距離11に保持し、4〜27秒で距離1e-26へ移動します。最初と最後のフレームを含むため、最終indexは1620です。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-quad-zoom.yaml
```

出力先は既定で`output/mandelbox-quad-zoom/`、ファイル名は`frame_000000.png`〜`frame_001620.png`です。別のディレクトリへ出す場合、animation sceneの`--output`はPNGファイルではなくディレクトリを指定します。このサンプルにはPhase 4の`video`設定もあるため、連番完了後に`output/mandelbox-quad-zoom.mp4`も生成します。PNGだけが必要な場合は`--no-video`を指定します。

深いフレームだけを確認・再生成できます。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-quad-zoom.yaml \
  --frame 1620 \
  --output output/phase3-check
```

中断した連番は`--resume`で再開します。既存PNGは全体をdecodeし、sceneと同じ解像度で正常な場合だけskipします。破損・解像度不一致は黙って飛ばさずエラーになるため、そのフレームを置換する場合は`--overwrite`を使います。`--resume`と`--overwrite`は同時指定できません。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-quad-zoom.yaml --resume

cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-quad-zoom.yaml --frame 1620 --overwrite
```

各PNGは一時ファイルへのencode完了後にrenameされるため、中断時に未完成のフレームを完成済みとして扱いません。quad-float Mandelboxでは各フレームのcamera distanceに合わせてDE反復数、`max_distance`、`epsilon`を再計算し、GPU pipeline・texture・readback bufferは連番全体で再利用します。経路は`renderer-core/src/path.rs`、timelineとcamera合成は`renderer-core/src/animation.rs`に分離してあり、予定している経路選択アルゴリズムからも再利用できます。

RTX 3070 / GL backendの320x180回帰テストでは、同一rendererで始点・中間・終点を連続描画し、距離1e-26の終点まで有効な色分布を確認しています。Phase 3時点の1 spp・追加効果offでは、実測した単一フレーム時間は0.42〜0.45秒でした。

## ターゲット周回経路

`target-orbit`はsceneの`camera.target`を常に注視し、一定距離の球面上でその周囲を周回します。`axis`と、軸からtarget→camera方向までの`cone_angle_degrees`で軌道面を指定します。90度なら大円、90度未満ならcameraが軸側に留まる小円となり、camera→targetの視線は`-axis`を中心とする円錐に沿います。たとえば`axis: [0, 0, 1]`と35度の組み合わせでは、cameraはtargetより上に留まって見下ろし続けます。

```yaml
animation:
  fps: 30
  frame_count: 361
  path:
    kind: target-orbit
    parameters:
      radius: 4.2
      duration: 12.0
      revolutions: 1.0
      axis: [0.0, 0.0, 1.0]
      cone_angle_degrees: 35.0
      start_angle_degrees: 0.0
```

`radius`は正のcamera距離、`duration`は軌道を進み終える秒数です。`axis`は方向だけが使われ、自動的に正規化されます。`revolutions`は周回数で、負値にすると逆回転し、小数なら部分周回になります。`start_angle_degrees`の0度方向は、初期`camera.position - camera.target`を`axis`に垂直な平面へ射影して決まります。このため初期cameraを基準に開始位相を直感的に調整できます。cameraの`up`は軸を画面へ射影した方向へ毎frame更新され、周回中の不要なrollを抑えます。実行可能な例は`scenes/examples/mandelbulb-target-orbit.yaml`です。

## 自動経路探索

portfolio由来の`pickOriginGapDir()`は以前から、+X付近へ96本のCPUレイを投げ、原点へ最も深く到達した命中点を組み込みMandelboxの初期構図に使用していました。ただし静止画preset専用で、sceneのanimationからは選べませんでした。現在は全候補を評価する`TargetPicker::pick_best()`を追加し、Mandelbulb、Mandelbox、typed DSLのCPU DEから次の2経路を事前計画できます。同じscene seedなら探索結果も同一です。

`multi-target-dive`は候補の奥行き、面の見やすい角度、照明方向をscore化して全探索レイからターゲットを選びます。overviewから指数ズームした後、暗転中に次の事前計画済みターゲットへ切り替えて繰り返します。必要ターゲット数はfps、frame count、各durationから自動算出され、描画中にCPU探索を繰り返しません。

```yaml
animation:
  fps: 30
  frame_count: 870
  path:
    kind: multi-target-dive
    parameters:
      overview_distance: 7.2
      minimum_distance: 1.0e-4
      overview_duration: 2.0
      dive_duration: 7.0
      transition_duration: 1.0
      search:
        bound_radius: 4.8
        hit_epsilon: 8.0e-7
        max_steps: 1000
        attempts: 192
        aim_jitter: 0.30
```

サンプル`mandelbox-multi-target-dive.yaml`は経路確認用の既存materialを流用せず、「Midnight Opal」をテーマに新規設計しています。scale -2.36、fold limit 1.06のMandelbox派生をtyped DSLで構成し、黒曜石色の基材へ狭いcyan、magenta、amberのorbit palette帯を配置しました。168乗の冷白色highlightと24乗のpalette着色highlight、強いAO、暗紫色の背景を組み合わせ、3つの自動ターゲットを巨大な異なる建築物として見せます。29秒弱のシーケンスは2回の暗転切替後、3番目の最深部で終了します。

`surface-flyover`は探索した表面のDE勾配を局所的な鉛直法線とし、`travel_direction`を接平面へ射影します。さらに接平面内の16方向について経路上へ各12本の確認レイを投げ、同じ向きの面が最も長く続く方向を採用します。cameraは`camera_height`を維持し、接平面内をsmoothstepで移動します。`look_ahead: 0.0`なら真下、それより大きい値では進行方向を少し見下ろします。十分な面を確認できない場合は、空を映したままrenderせずscene読込時にエラーにします。

```yaml
animation:
  fps: 30
  frame_count: 541
  path:
    kind: surface-flyover
    parameters:
      camera_height: 1.25
      travel_distance: 2.9
      duration: 18.0
      look_ahead: 0.0
      travel_direction: [0.2, 1.0, 0.35]
      normal_epsilon: 1.5e-4
      search:
        bound_radius: 5.0
        hit_epsilon: 8.0e-7
        max_steps: 1000
        attempts: 192
        aim_jitter: 0.28
```

サンプル`mandelbox-surface-flyover.yaml`は「Verdigris Atlas」をテーマにした独自sceneです。scale -1.86と広いfoldで上空から読みやすい段丘を作り、orbit coloringによる深緑、緑青、象牙、珊瑚色の領域を地表へ固定しました。暖色の低い斜光、強いAO、粗い半光沢によって、航空写真や古い立体地図のような質感を狙っています。cameraは局所法線を真下に取り、18秒かけて接平面内を2.9 world単位移動します。終点まで地表を画面内に維持し、細密な反射を動画で安定させるため32 sppを使用します。

実行可能なsceneは`scenes/examples/mandelbox-multi-target-dive.yaml`と`scenes/examples/mandelbox-surface-flyover.yaml`です。まず`--frame`と低解像度overrideで始点・切替後・終点を確認できます。

```bash
WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-multi-target-dive.yaml \
  --frame 300 --width 640 --height 360 --output output/path-proof --overwrite

WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-surface-flyover.yaml \
  --frame 540 --width 640 --height 360 --output output/flyover-proof --overwrite
```

自動探索経路は現在f32専用です。quad-float Mandelboxの超深度shaderは解析的な+X境界への座標rebaseを前提とするため、任意の探索ターゲットをそのまま使うと精度保証が崩れるためです。

## FFmpeg による動画生成（Phase 4・実装済み）

animation sceneへ任意の`video`セクションを追加すると、全PNGの生成・decode・解像度検証後にFFmpegを実行します。省略フィールドには次の既定値を使えます。

```yaml
video:
  codec: libx264
  pixel_format: yuv420p
  crf: 18
  preset: slow
  faststart: true
```

内部では次と同等の引数をshellを介さずFFmpegへ渡し、sceneのFPSとframe countを固定します。

```bash
ffmpeg \
  -framerate 60 \
  -start_number 0 \
  -i output/scene-name/frame_%06d.png \
  -frames:v 1621 \
  -c:v libx264 \
  -pix_fmt yuv420p \
  -crf 18 \
  -preset slow \
  -movflags +faststart \
  output/scene-name.mp4
```

sceneに`video`がないanimationでも`--video`で既定設定を有効化できます。各項目と出力先はCLIから上書きできます。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-quad-zoom.yaml \
  --video-output output/mandelbox-custom.mp4 \
  --video-codec libx264 \
  --video-pixel-format yuv420p \
  --video-crf 20 \
  --video-preset medium
```

中断後にPNGを再利用して動画だけ作り直す場合は、`--resume --video-overwrite`を組み合わせます。`--overwrite`はPNGと動画の両方を再生成します。

```bash
cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-quad-zoom.yaml \
  --resume --video-overwrite
```

`--frame`は部分シーケンスなので、scene由来の動画encodeを自動的にskipします。`--frame`と明示的な`--video`または動画overrideを同時指定した場合はエラーにします。`yuv420p`は縦横とも偶数の解像度を事前要求します。

FFmpegの存在とversionは長時間renderの開始前に確認します。encode開始前には全PNGを完全decodeし、欠落・破損・解像度不一致があればFFmpegを起動しません。動画は同じcontainer拡張子を持つ一時ファイルへ生成して成功後にrenameするため、FFmpeg失敗時もPNG連番と既存動画を削除・破損しません。

## Offline quality（Phase 5・実装済み）

`scenes/examples/mandelbox.yaml`は16 sppの静止画、`scenes/examples/mandelbox-quad-zoom.yaml`は8 sppの動画向け設定例です。全sampleはlinear HDRで加算・平均し、tone mappingを1画素につき最後の1回だけ適用します。`Rgba8UnormSrgb`への書き込み時にGPUがlinearからsRGBへ変換するため、shader内でgammaを二重適用しません。

```yaml
camera:
  # ほかのcamera fieldは省略
  aperture_radius: 0.12  # world単位。0.0でpinhole camera
  focus_distance: 11.0   # lens面から合焦面までの距離

quality:
  samples_per_pixel: 16  # 1..128
  ambient_occlusion:
    max_steps: 64        # 0でoff、上限256
    radius: 1.25
    strength: 0.72
  soft_shadow:
    max_steps: 96        # 0でoff、上限256
    angular_radius_degrees: 1.5
    max_distance: 16.0
  reflection:
    max_steps: 96        # 0またはstrength 0でoff、上限256
    max_distance: 14.0
    strength: 0.12
    roughness: 0.08
  tone_mapping:
    enabled: true
    operator: extended-reinhard
    exposure_stops: -0.35
    white_point: 3.0
  post_process:
    enabled: true
    exposure_stops: 0.0    # tone mapping前の追加露出（-20..20 EV）
    contrast: 1.0          # 0..4、1.0で変化なし
    saturation: 1.0        # 0..4、1.0で変化なし
    gamma: 1.0             # 0.1..4、1.0で変化なし
    vignette_strength: 0.0 # 0..1
```

camera rayは画素内をjitterし、`aperture_radius`の円板上から`focus_distance`の合焦面へ向け直します。円形開口なので、焦点外のhighlightは円形ボケとして蓄積されます。soft shadowはdirectional lightの角半径内、AOは法線半球内、rough reflectionは反射方向の周囲を同じ決定的seedから分散samplingします。noiseが見える場合はまず`samples_per_pixel`を増やしてください。被写界深度を強くするには`aperture_radius`を増やし、合焦位置は`focus_distance`で調整します。

quad-float animationではcamera distanceの変化に合わせ、`focus_distance`、`aperture_radius`、AO半径、shadow/reflection trace距離を同じ比率で自動scaleします。このため1e-26までzoomしても、world単位の効果範囲だけがoverview scaleに取り残されません。sampling上限と二次ray step上限はCPU validationとWGSLの固定長loopで一致させています。

`tone_mapping.operator`は既定の`extended-reinhard`に加え、`mandelbulber`を選択できます。後者はMandelbulber 2.26と同じく、`brightness`、`contrast`、HDR `tanh`、`saturation`、`gamma`の順に処理し、最後にsRGB render targetへ正しく渡します。既存sceneはoperatorを省略しても従来のReinhard表示を維持します。

`post_process`はtone operatorから独立した最終カラーグレードです。linear HDR上で`exposure_stops`を適用してからtone mappingし、表示sRGB上でcontrast、Rec. 709輝度基準のsaturation、gamma、vignetteの順に処理します。その後linearへ戻すため、sRGB render targetによる変換と二重gammaになりません。既定値は`enabled: false`で、各中立値は上の例のとおりです。`tone_mapping.exposure_stops`も有効な場合は露出が加算されるため、最終調整を`post_process`へ集約するsceneでは前者を`0.0`にしてください。

RTX 3070 / GL backend、320x180の実測では、16 sppのf32静止画が0.72秒、8 sppで全効果を有効にしたquad-float距離1e-26の最終frameが1.52秒でした。GL driverでquad-float pipelineを初めて作る際は約60秒のshader compileが発生しましたが、animation中は同じpipelineを全frameで再利用するためframe時間には含まれません。数値はGPU・backend・driverで変わります。

設計と実装では次の一次文献を参照しました。

- Cook, Porter, Carpenter, [Distributed Ray Tracing](https://graphics.pixar.com/library/DistributedRayTracing/paper.pdf)（画素・lens・light・反射方向の分散sampling、被写界深度、soft shadow）
- Whitted, [An Improved Illumination Model for Shaded Display](https://doi.org/10.1145/358876.358882)（shadow rayとspecular secondary ray）
- Hart, [Sphere Tracing: A Geometric Method for the Antialiased Ray Tracing of Implicit Surfaces](https://doi.org/10.1007/s003710050084)（SDF/DEの一次・二次visibility query）
- Bunnell, [Dynamic Ambient Occlusion and Indirect Lighting](https://developer.nvidia.com/gpugems/gpugems2/part-ii-shading-lighting-and-shadows/chapter-14-dynamic-ambient-occlusion-and)（法線半球に対するaccessibilityとしてのAO）
- Reinhard et al., [Photographic Tone Reproduction for Digital Images](https://www-old.cs.utah.edu/docs/techreports/2002/pdf/UUCS-02-001.pdf)（white pointを持つextended global operator）

## Fractal DSL / AST（Phase 6・実装済み）

`fractal.kind: dsl`は、YAMLを`DslFractalConfig`と`OrbitTransform`の型付きASTへ変換し、validation後に`map`、material、background、atmosphereのWGSLを生成します。任意のWGSL文字列、関数名、変数名、loopは入力できません。生成部分は共通のcamera、Phase 5 sampling、shading、ray marcherと合成されます。

```yaml
fractal:
  kind: dsl
  parameters:
    iterations: 18
    normal_epsilon: 5.0e-5
    orbit:
      - op: rotate
        axis: [0.3, 0.8, 1.0]
        degrees: 7.5
      - op: box-fold
        limit: 1.14
      - op: sphere-fold
        min_radius_squared: 0.60
        fixed_radius_squared: 2.65
      - op: scale-add-point
        scale: -2.18
    material:
      base_color: [0.035, 0.16, 0.48]
      accent_color: [1.25, 0.28, 0.035]
      color_frequency: 8.0
      camera_palette_weight: 1.0
      normal_palette_weight: 0.15
      shininess: 56.0
```

利用可能なorbit命令は次の5種類です。

- `box-fold`: 軸ごとのbox fold
- `sphere-fold`: 最小・固定半径によるsphere foldとDE derivative更新
- `scale-add-point`: `z = scale * z + p`とDE derivative更新
- `rotate`: 任意axisのRodrigues rotation
- `translate`: 定数offsetの加算

DSLはf32専用です。`iterations`は1〜96、orbitは1〜16命令、`scale-add-point`は正確に1個、scaleの絶対値は1.000001〜4.0へ制限されます。色、半径、角度、移動量などもfiniteな範囲内か検証し、未知fieldと`wgsl`のようなraw shader fieldはscene schemaで拒否します。AST定数は生成WGSLへ埋め込まれるため、animation中にASTを変更して既存pipelineを再利用しようとした場合もrendererが拒否します。

サンプルを次のコマンドで描画できます。

```bash
WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/twisted-mandelbox-dsl.yaml \
  --output output/twisted-mandelbox-dsl.png \
  --overwrite
```

Rust側の再利用可能なAST・generatorは`renderer-core/src/dsl.rs`、同じASTを解釈するCPU `DistanceEstimator`は`renderer-core/src/fractal.rs`、YAMLとの変換は`renderer-core/src/scene_file.rs`、shader moduleへの合成は`renderer-core/src/shader.rs`に分離しています。DSL programは既存の`TargetPicker`へそのまま渡せます。新しい安全な命令を追加するときは、AST variant、domain validation、CPU解釈、固定templateによるWGSL生成、scene変換を同時に追加します。

## 最初のYouTube作品用scene

`scenes/examples/mandelbox-first-descent-youtube.yaml`は、投稿前の微調整を始めるための公開品質masterです。16:9の1440p60、24秒、32 sppとし、最初の3秒で全体像を見せた後、解析的な+X境界へ指数的に潜ります。正面対称の構図により、中盤の格子、円環、深部の自己相似模様が画面中央で連続して現れます。

色は暗いsapphireと暖色のcopperを対置しました。深部でworld座標の差が小さくなっても単色化しないよう、DSL materialの`camera_palette_weight`で合焦距離により正規化したcamera相対paletteを合成し、`normal_palette_weight`で法線による変化も加えます。またf32 animationでも、focus distance、aperture、AO、soft shadow、reflectionの距離をcamera distanceに比例させるため、被写界深度と二次効果の見かけの大きさがズーム中に維持されます。

まず始点・中間・後半・終点を低解像度で確認します。`--frame`ではsceneの動画encodeが自動的にskipされます。

```bash
for frame in 0 720 1200 1440; do
  WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
    scenes/examples/mandelbox-first-descent-youtube.yaml \
    --frame "$frame" --width 640 --height 360 \
    --output output/mandelbox-first-descent-proof --overwrite
done
```

構図を確定したら、次のコマンドでPNG連番とMP4を生成します。PNG連番を編集・再encode用のmasterとして保持し、MP4はYouTubeへ直接uploadできるH.264 / `yuv420p` / CRF 14 / fast-start設定です。

```bash
WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/mandelbox-first-descent-youtube.yaml
```

途中から再開する場合は`--resume`、PNGを再利用してMP4だけ作り直す場合は`--resume --video-overwrite`を使います。[YouTubeの解像度とaspect ratioの案内](https://support.google.com/youtube/answer/6375112?co=GENIE.Platform%3DDesktop&hl=ja)にある標準16:9の2560x1440を採用し、[推奨upload encode設定](https://support.google.com/youtube/answer/1722171?hl=ja)に合わせて撮影時と同じ60 fps、MP4、H.264、4:2:0、fast-start構成にしています。YouTube側で再圧縮されるため、sceneのCRFは配信用bitrateではなく入力masterの品質を優先しています。

## Alchemy PseudoKleinian scene

`scenes/examples/alchemy-pseudo-kleinian.yaml`は、Mandelbulber 2.26の`alchemy.fract`と参照画像を基にした3:2の静止画sceneです。設定ファイルのformula ID 73は[公式enumではAmazing Surf](https://github.com/buddhi1980/mandelbulber2/blob/2.26/mandelbulber2/formula/definition/all_fractal_list_enums.hpp)、ID 8はMandelboxです。[Amazing Surfの公式実装](https://github.com/buddhi1980/mandelbulber2/blob/2.26/mandelbulber2/formula/definition/fractal_amazing_surf.cpp)はKaliによるPseudoKleinian派生のX/Y foldであるため、単一のMandelboxへ近似せず、120回周期のhybrid sequenceをgeometryではN=125まで評価します。

```text
iteration   0..19   Amazing Surf + Y 90° rotation
iteration  20..119  rotated Mandelbox + fixed Julia constant
iteration 120..124  Amazing Surf + Y 90° rotation
```

DSLへ`amazing-surf-fold`と`mandelbox-julia-fold`を追加し、それぞれに閉区間ではなく`start_iteration <= i < stop_iteration`の実行範囲と`orbit_period`を持たせています。camera、target、up、FOV、focus distance、Julia constant、fold、scale、rotationは元設定から移植しました。

[公式のfractal coloring](https://github.com/buddhi1980/mandelbulber2/blob/2.26/mandelbulber2/src/fractal_coloring.cpp)と[surface color shader](https://github.com/buddhi1980/mandelbulber2/blob/2.26/mandelbulber2/src/shader_surface_color.cpp)に合わせ、着色時だけN×4=500回まで周期を継続し、各軸のbox foldとsphere foldから補助色を累積します。元gradientの色順を維持しつつ、実装間で異なる補助色分布を補うため、銅・古金色を広げて銀・白金色を狭い帯へ再配分しました。[公式specular shader](https://github.com/buddhi1980/mandelbulber2/blob/2.26/mandelbulber2/src/shader_specular_highlight_combined.cpp)と同じく、細い白色plastic highlightと広い表面色metallic highlightを別々に評価します。

光源方向は`light1_rotation: [25.76, 37.98, 0]`を元camera basisで変換した値を使用し、直接光量0.8もdiffuseと金属反射へ反映しています。画像階調は[公式の画像処理順序](https://github.com/buddhi1980/mandelbulber2/blob/2.26/mandelbulber2/src/cimage.cpp)を再利用可能な`mandelbulber` tone operatorとして実装し、Alchemy sceneではこのレンダラーのAO差に合わせてbrightness、contrast、saturationを補正しました。さらにoperator非依存の`post_process`でわずかな追加露出、彩度、contrast、vignetteを調整しています。被写界深度はfocus distanceを維持したままapertureを0.015に設定しています。

sceneの標準解像度は元画像と同じ1620x1080、samplingは128 sppです。まず構図と色を確認する場合は下のように405x270へoverrideできます。RTX 3070 / GL backendでのpreview実測は約4.8秒です。

```bash
WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/alchemy-pseudo-kleinian.yaml \
  --width 405 --height 270 \
  --output output/alchemy-pseudo-kleinian-preview.png --overwrite
```

`scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml`は、視線中央の装飾表面を`camera.target`に固定した24秒・30 fpsの周回作例です。frame 0の`camera.up`を鉛直軸とする大円上を50度進むため、画面を水平に保ちながら元の正面構図から対象の側面奥へ回り込みます。180度の裏側まで回るとcameraがDE表面内へ入るため、composition previewで形状密度を比較し、対象を見失わない50度を終点にしました。fractal geometry、orbit palette、material、world-space light、FOV、tone mapping、post process、128 spp設定は静止画sceneと同一です。

```bash
WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- render \
  scenes/examples/alchemy-pseudo-kleinian-target-orbit.yaml \
  --frame 180 --width 405 --height 270 \
  --output output/alchemy-target-orbit-preview --overwrite --no-video
```

## ディレクトリ構成

```text
.
├── Cargo.toml                    # Cargo workspace
├── renderer-core/
│   ├── camera.wgsl              # camera ray と共通関数
│   ├── quality.wgsl             # sampling、方向分布、HDR tone mapping
│   ├── raymarch.wgsl            # fullscreen triangle と sphere tracing
│   ├── shading.wgsl             # normal と lighting
│   ├── fractal/
│   │   ├── mandelbulb.wgsl      # Mandelbulb map / material
│   │   ├── mandelbox.wgsl       # Portfolio Mandelbox f32 map / material
│   │   └── mandelbox_quad.wgsl  # quad-float Mandelbox map / material
│   ├── precision/                # WGSL Qf32 / QfVec3
│   └── src/
│       ├── fractal.rs           # CPU DistanceEstimator
│       ├── dsl.rs               # typed AST / validation / WGSL generator
│       ├── path.rs              # target search / dive / surface flyover
│       ├── animation.rs         # timeline / 自動経路 / camera合成
│       ├── video.rs             # codec設定とscene validation
│       ├── precision/            # CPU Qf32 / QfVec3
│       ├── scene.rs             # scene presets
│       ├── scene_file.rs        # versioned YAML schema / validation
│       └── ...                  # config、wgpu、readback
├── renderer-cli/
│   └── src/                      # workflowを利用する人間向けCLI adapter
├── renderer-workflow/
│   └── src/                      # 共通frame/FFmpeg実行、revision、job、preview、render、artifact
├── renderer-harness/
│   └── src/                      # LLM向けJSONL tool protocolとresource policy
├── docs/
│   ├── agent-harness.md          # tool-call運用手順
│   └── agent-harness-roadmap.md  # 優先順位、移行順、完了条件
├── scenes/examples/             # version 1 のサンプル scene
└── output/                       # 生成物（Git 管理外）
```

## 後続フェーズ

1. Phase 2（完了）: YAML scene schema、読み込み、validation
2. Phase 2.5（完了）: 4×`f32` quad-float、`QfVec3`、高精度camera/DE/path、精度・性能検証
3. Phase 3（完了）: quad-float対応animation、指数ズーム、連番、resume、`--frame`、`--overwrite`
4. Phase 4（完了）: configurable FFmpeg integration、検証付き動画encode
5. Phase 5（完了）: HDR accumulation、AO、soft shadow、reflection、thin-lens DOF、tone mapping
6. Phase 6（完了）: typed fractal DSL/AST、限定的なWGSL生成とvalidation
