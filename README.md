# Fractal Movie

Rust、wgpu、WGSL で3次元フラクタルを描画する、ウィンドウ不要のオフスクリーンレンダラーです。fullscreen triangle の fragment shader から Mandelbulb または Mandelbox の Distance Estimator を ray marching し、PNG へ保存できます。

## 現在の実装範囲

- wgpu による headless adapter/device 初期化（画面・surface 不要）
- WGSL による Mandelbulb / Mandelbox、sphere tracing、法線、フラクタル別ライティング
- CPU Distance Estimator による再利用可能なカメラターゲット探索
- CPU/WGSL共通の4×`f32` quad-float座標と高精度Mandelbox DE
- 十進文字列または4 limb展開を保持できる高精度scene camera
- overview から深部へ進む指数ズーム経路モデル
- `Rgba8UnormSrgb` オフスクリーン texture から row alignment を考慮した readback
- PNG 出力を GPU renderer から分離
- 解像度、ray steps、fractal iterations、NaN/Inf などの事前 validation
- WGSL validation error を文脈付きエラーとして報告
- GPU 名、解像度、フレーム時間、合計時間のログ

animation、resume、FFmpeg 自動実行は Phase 3〜4 の対象で、まだ CLI からは利用できません。

## 必要環境

- Rust 1.87 以上（`rust-toolchain.toml` は検証済みの 1.97.1 を指定）
- Vulkan / Direct3D 12 / Metal / OpenGL ES のいずれかを利用できる wgpu 対応 driver
- PNG 1枚の出力には FFmpeg は不要
- 後述の動画変換には FFmpeg

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

主要フィールドは `fractal.kind/parameters`、`camera.position/target/up/vertical_fov_degrees`、`light.direction`、`render.width/height/max_steps/max_distance/epsilon/step_safety/pixel_epsilon_multiplier`、`seed`、`precision` です。未知の version・フィールドや範囲外の値はエラーにし、自由な WGSL 全体は scene に埋め込みません。

scene の値は、明示した CLI オプションだけで上書きできます。

```bash
cargo run --release -p fractal-renderer-cli -- \
  render scenes/examples/mandelbox.yaml \
  --width 1920 --height 1080 --seed 20260811 \
  --output output/mandelbox-1080p.png
```

`--output` を省略した scene file の出力先は `output/<scene名>/<scene名>.png` です。`precision: quad-float` はMandelboxで利用できます。現在Mandelbulbをquad-floatへ変更すると、未対応であることを明示したエラーになります。

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

## Animation と任意フレーム（Phase 3）

uniform layout にはすでに frame index と time があり、`Renderer::render_frame(frame, time)` へ渡せます。Phase 3 ではPhase 2.5の`QfVec3`対応camera/path APIを前提として、scene の FPS と frame 数から `time = frame / fps` を求め、`ExponentialDivePath`による超高倍率ズーム、連番 PNG、`--frame 120`、既存フレームの skip、明示的な `--overwrite` を CLI に追加します。

## FFmpeg による動画生成（Phase 4）

Phase 3 の連番画像が `frame_%06d.png` として生成された後は、renderer と codec を分離したまま次のように MP4 化します。

```bash
ffmpeg \
  -framerate 60 \
  -i output/scene-name/frame_%06d.png \
  -c:v libx264 \
  -pix_fmt yuv420p \
  output/scene-name.mp4
```

Phase 4 ではこの subprocess 呼び出しと codec options を scene/CLI 設定へ追加します。FFmpeg が失敗しても生成済み PNG は削除しない設計にします。

## ディレクトリ構成

```text
.
├── Cargo.toml                    # Cargo workspace
├── renderer-core/
│   ├── camera.wgsl              # camera ray と共通関数
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
│       ├── precision/            # CPU Qf32 / QfVec3
│       ├── scene.rs             # scene presets
│       ├── scene_file.rs        # versioned YAML schema / validation
│       └── ...                  # config、wgpu、readback
├── renderer-cli/
│   └── src/                      # CLI と PNG encoder
├── scenes/examples/             # version 1 のサンプル scene
└── output/                       # 生成物（Git 管理外）
```

## 後続フェーズ

1. Phase 2（完了）: YAML scene schema、読み込み、validation
2. Phase 2.5（完了）: 4×`f32` quad-float、`QfVec3`、高精度camera/DE/path、精度・性能検証
3. Phase 3: quad-float対応animation、指数ズーム、連番、resume、`--frame`、`--overwrite`
4. Phase 4: configurable FFmpeg integration
5. Phase 5: accumulation、AO、soft shadow、reflection、HDR/tone mapping
6. Phase 6: fractal DSL/AST、限定的な WGSL 生成と validation
