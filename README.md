# Fractal Movie

Rust、wgpu、WGSL で3次元フラクタルを描画する、ウィンドウ不要のオフスクリーンレンダラーです。fullscreen triangle の fragment shader から Mandelbulb または Mandelbox の Distance Estimator を ray marching し、PNG へ保存できます。

## 現在の実装範囲

- wgpu による headless adapter/device 初期化（画面・surface 不要）
- WGSL による Mandelbulb / Mandelbox、sphere tracing、法線、フラクタル別ライティング
- CPU Distance Estimator による再利用可能なカメラターゲット探索
- overview から深部へ進む指数ズーム経路モデル
- `Rgba8UnormSrgb` オフスクリーン texture から row alignment を考慮した readback
- PNG 出力を GPU renderer から分離
- 解像度、ray steps、fractal iterations、NaN/Inf などの事前 validation
- WGSL validation error を文脈付きエラーとして報告
- GPU 名、解像度、フレーム時間、合計時間のログ

quad-float高精度座標、animation、resume、FFmpeg 自動実行は Phase 2.5〜4 の対象で、まだ CLI からは利用できません。

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

`--output` を省略した scene file の出力先は `output/<scene名>/<scene名>.png` です。`precision: quad-float` はスキーマ上で予約済みですが、`f32` へ黙ってフォールバックせず、Phase 2.5 が実装されるまでは明示的なエラーになります。

## Quad-float 高精度座標（Phase 2.5）

Phase 3の経路アニメーションより先に、4個の`f32`を非重複な展開として保持するquad-float座標基盤を実装します。汎用IEEE 754任意精度floatではなく、Mandelboxの超高倍率ズームに必要な演算へ対象を限定します。

### 実装範囲

1. `precision`モジュールに`Qf32`と`QfVec3`を定義し、正規化、比較、加減算、乗算、除算、内積をCPUとWGSLへ実装する
2. cameraの基準点、相対オフセット、ray上の位置、Mandelbox反復座標をquad-float化する
3. 色、時間、ライト、正規化後の法線など、精度へ影響しない値は`f32`のまま維持する
4. `PathTarget`の`[f64; 3]`固定を高精度座標型へ置き換え、ターゲット探索と`ExponentialDivePath`から`f32`への早期丸めをなくす
5. 予約済みのscene schemaの`precision`を実行時の座標型へ接続し、高精度用Mandelbox sceneはquad-floatを選択する
6. CPU側の高精度参照値とWGSL結果を比較する演算テスト、ズーム深度別のDEテスト、golden imageテスト、GPU性能測定を追加する

### 完了条件

- cameraからshaderまでの座標経路に意図しない`f64 → f32`変換がない
- quad-floatの各演算がCPU高精度oracleに対する誤差基準を満たす
- 複数のズーム深度で表面の連続性、法線、同一seedの再現性を確認できる
- 実測で保証できる最大ズーム倍率とGPUコストが文書化されている
- Phase 3はこの条件を満たした高精度camera/path APIだけを利用する

想定する配置は次のとおりです。

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
│   │   └── mandelbox.wgsl       # Portfolio Mandelbox map / material
│   └── src/
│       ├── fractal.rs           # CPU DistanceEstimator
│       ├── path.rs              # target search / dive path
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
2. Phase 2.5: 4×`f32` quad-float、`QfVec3`、高精度camera/DE/path、精度・性能検証
3. Phase 3: quad-float対応animation、指数ズーム、連番、resume、`--frame`、`--overwrite`
4. Phase 4: configurable FFmpeg integration
5. Phase 5: accumulation、AO、soft shadow、reflection、HDR/tone mapping
6. Phase 6: fractal DSL/AST、限定的な WGSL 生成と validation
