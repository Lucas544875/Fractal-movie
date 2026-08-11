# Fractal Movie

Rust、wgpu、WGSL で3次元フラクタルを描画する、ウィンドウ不要のオフスクリーンレンダラーです。現在は Phase 1 として、fullscreen triangle の fragment shader から Mandelbulb の Distance Estimator を ray marching し、法線・ディレクショナルライトを計算して PNG 1枚へ保存できます。

## 現在の実装範囲

- wgpu による headless adapter/device 初期化（画面・surface 不要）
- WGSL による Mandelbulb、sphere tracing、法線、基本ライティング
- `Rgba8UnormSrgb` オフスクリーン texture から row alignment を考慮した readback
- PNG 出力を GPU renderer から分離
- 解像度、ray steps、fractal iterations、NaN/Inf などの事前 validation
- WGSL validation error を文脈付きエラーとして報告
- GPU 名、解像度、フレーム時間、合計時間のログ

外部 scene file、animation、resume、FFmpeg 自動実行は Phase 2〜4 の対象で、まだ CLI からは利用できません。

## 必要環境

- Rust 1.87 以上（`rust-toolchain.toml` は検証済みの 1.97.1 を指定）
- Vulkan / Direct3D 12 / Metal / OpenGL ES のいずれかを利用できる wgpu 対応 driver
- PNG 1枚の出力には FFmpeg は不要
- 後述の動画変換には FFmpeg

Linux の headless 環境では Vulkan loader と対象 GPU の Vulkan driver が必要です。利用可能な hardware adapter がない環境では、wgpu が software Vulkan adapter を選ぶ場合があります。選択結果は起動時の `GPU:` ログで確認できます。

## Build と検証

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
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

## Scene file（Phase 2）

renderer はすでに `RenderConfig` と GPU 実装を分離しています。Phase 2 では versioned YAML schema から camera、fractal、light、render、seed を deserialize・validate し、次の形で起動できるようにします。

```bash
fractal-render render scene/examples/mandelbulb.yaml
```

予定する主要フィールドは `fractal.power/iterations/bailout`、`camera.position/target/fov`、`render.width/height/max_steps/max_distance/epsilon`、`seed` です。自由な WGSL 全体を scene に埋め込まず、将来も `fn map(p: vec3<f32>) -> f32` の限定された生成境界を使います。

## Animation と任意フレーム（Phase 3）

uniform layout にはすでに frame index と time があり、`Renderer::render_frame(frame, time)` へ渡せます。Phase 3 では scene の FPS と frame 数から `time = frame / fps` を求め、連番 PNG、`--frame 120`、既存フレームの skip、明示的な `--overwrite` を CLI に追加します。

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
│   │   └── mandelbulb.wgsl      # 差し替え可能な map(p) 実装
│   └── src/                      # config、wgpu、readback
├── renderer-cli/
│   └── src/                      # CLI と PNG encoder
└── output/                       # 生成物（Git 管理外）
```

## 後続フェーズ

1. Phase 2: YAML scene schema、読み込み、validation
2. Phase 3: animation、連番、resume、`--frame`、`--overwrite`
3. Phase 4: configurable FFmpeg integration
4. Phase 5: accumulation、AO、soft shadow、reflection、HDR/tone mapping
5. Phase 6: fractal DSL/AST、限定的な WGSL 生成と validation

