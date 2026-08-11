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
- `Rgba8UnormSrgb` オフスクリーン texture から row alignment を考慮した readback
- PNG 出力を GPU renderer から分離
- 解像度、ray steps、fractal iterations、NaN/Inf などの事前 validation
- WGSL validation error を文脈付きエラーとして報告
- GPU 名、解像度、フレーム時間、合計時間のログ

Phase 5まで実装済みで、offline品質の光学・ライティング効果を含むPNG連番とFFmpeg動画encodeをCLIから一続きで実行できます。

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

経路関連のコードはGPUレンダラーから独立しています。`DistanceEstimator` を実装したフラクタルは `TargetPicker` による表面ターゲット探索を再利用でき、`ExponentialDivePath::distance_at()` は参照JSの overview→dive の距離曲線を提供します。将来の連番アニメーションでは、この結果を各フレームのcamera設定へ渡せます。

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
  samples_per_pixel: 16  # 1..64
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
    exposure_stops: -0.35
    white_point: 3.0
```

camera rayは画素内をjitterし、`aperture_radius`の円板上から`focus_distance`の合焦面へ向け直します。円形開口なので、焦点外のhighlightは円形ボケとして蓄積されます。soft shadowはdirectional lightの角半径内、AOは法線半球内、rough reflectionは反射方向の周囲を同じ決定的seedから分散samplingします。noiseが見える場合はまず`samples_per_pixel`を増やしてください。被写界深度を強くするには`aperture_radius`を増やし、合焦位置は`focus_distance`で調整します。

quad-float animationではcamera distanceの変化に合わせ、`focus_distance`、`aperture_radius`、AO半径、shadow/reflection trace距離を同じ比率で自動scaleします。このため1e-26までzoomしても、world単位の効果範囲だけがoverview scaleに取り残されません。sampling上限と二次ray step上限はCPU validationとWGSLの固定長loopで一致させています。

RTX 3070 / GL backend、320x180の実測では、16 sppのf32静止画が0.72秒、8 sppで全効果を有効にしたquad-float距離1e-26の最終frameが1.52秒でした。GL driverでquad-float pipelineを初めて作る際は約60秒のshader compileが発生しましたが、animation中は同じpipelineを全frameで再利用するためframe時間には含まれません。数値はGPU・backend・driverで変わります。

設計と実装では次の一次文献を参照しました。

- Cook, Porter, Carpenter, [Distributed Ray Tracing](https://graphics.pixar.com/library/DistributedRayTracing/paper.pdf)（画素・lens・light・反射方向の分散sampling、被写界深度、soft shadow）
- Whitted, [An Improved Illumination Model for Shaded Display](https://doi.org/10.1145/358876.358882)（shadow rayとspecular secondary ray）
- Hart, [Sphere Tracing: A Geometric Method for the Antialiased Ray Tracing of Implicit Surfaces](https://doi.org/10.1007/s003710050084)（SDF/DEの一次・二次visibility query）
- Bunnell, [Dynamic Ambient Occlusion and Indirect Lighting](https://developer.nvidia.com/gpugems/gpugems2/part-ii-shading-lighting-and-shadows/chapter-14-dynamic-ambient-occlusion-and)（法線半球に対するaccessibilityとしてのAO）
- Reinhard et al., [Photographic Tone Reproduction for Digital Images](https://www-old.cs.utah.edu/docs/techreports/2002/pdf/UUCS-02-001.pdf)（white pointを持つextended global operator）

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
│       ├── path.rs              # target search / dive path
│       ├── animation.rs         # timeline / quad-float camera合成
│       ├── video.rs             # codec設定とscene validation
│       ├── precision/            # CPU Qf32 / QfVec3
│       ├── scene.rs             # scene presets
│       ├── scene_file.rs        # versioned YAML schema / validation
│       └── ...                  # config、wgpu、readback
├── renderer-cli/
│   └── src/                      # CLI、PNG encoder、FFmpeg subprocess
├── scenes/examples/             # version 1 のサンプル scene
└── output/                       # 生成物（Git 管理外）
```

## 後続フェーズ

1. Phase 2（完了）: YAML scene schema、読み込み、validation
2. Phase 2.5（完了）: 4×`f32` quad-float、`QfVec3`、高精度camera/DE/path、精度・性能検証
3. Phase 3（完了）: quad-float対応animation、指数ズーム、連番、resume、`--frame`、`--overwrite`
4. Phase 4（完了）: configurable FFmpeg integration、検証付き動画encode
5. Phase 5（完了）: HDR accumulation、AO、soft shadow、reflection、thin-lens DOF、tone mapping
6. Phase 6: fractal DSL/AST、限定的な WGSL 生成と validation
