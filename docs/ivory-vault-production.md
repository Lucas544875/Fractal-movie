# Ivory Vault — 制作記録

## 作品

象牙色の微細な彫刻と、青灰色・真鍮色の大きな曲面を、ゆっくり周回しながら覗き込む12秒のワンショット。中央の三葉状の装飾を視線の基点とし、背景と前景の視差で立体を見せる。音声、タイトル、カット、合成は含めない。

- 本番scene: `scenes/production/ivory-vault.yaml`
- 1920×1080、30 fps、360 frames、96 samples/pixel
- 30度の周回と、距離0.7911548236→0.57の接近を同時実行
- 発進・停止の速度と加速度を0にする5次補間
- 形状: Amazing Surf / Mandelbox Julia hybrid。既存のAlchemyの形状定義を利用し、材質、色、画角、レンズ、経路を新規に設計
- 色は表面のorbitに固定。カメラ依存のpaletteは使わない
- 細部の読みやすさを優先し、反射rayは使わず、AO、soft shadow、薄レンズDOFで奥行きを作る
- PNG連番をmasterとして保存し、H.264 / yuv420p / CRF 14 / slowで配布用MP4を生成

## 選定と検証

`output/ivory-vault/development/`に試作と検証結果を保存する。

1. 象牙単色、斜光、銀色、別のfold形状を比較。単色は形状の区別が弱く、別foldは塊の輪郭が強すぎたため不採用。
2. 広い象牙色と青灰色、控えめな真鍮色を持つpaletteを選定。20度の画角と小さな開口で中央の装飾を分離。
3. 一定距離の周回だけでは構図の展開が弱かったため、接近と滑らかな発進・停止を追加。
4. geometry/color反復数を125/240から64/120へ削減する案を試したが、細部と色が変わったため棄却。終点のSSIMは約0.802であり、同等品質とは扱わない。
5. 本番設定の始点・終点を1920×1080で描画し、細部とボケを確認。RTX 3070 / Mesa D3D12 GLで約56秒・41秒/枚。本番全尺はハーネスjob `job-1789779757818-1`で描画中。
6. 全360 frameのCPU DE検査を実行。最小clearance ratioは0.006548、設定した下限0.0001を上回る。これは離散フレームでのカメラ位置検査であり、全視線の可視性の証明ではない。
7. 640×360、16 sppの全尺プレビューを完成。360枚の欠落なし、12.000秒。全フレームの輝度検査では最大隣接平均輝度変化0.000602（0〜1）、白飛び画素率0。差分が最大の区間は中盤の最大速度区間と一致し、途中の構図も抽出画像で確認。実時間再生を視聴した評価ではない。

本番はimmutable revision `rev-93c2c3937e96ce5b`に固定。詳細は `output/ivory-vault/production-manifest.json`、検査結果は `development/route-inspection.json` と `development/motion-qc.json`。本番の全尺QCと最終ファイル情報は完成後に追記する。

## 制作中に追加した機能

`target-orbit`へ次の省略可能なフィールドを追加した。

- `end_radius`: 周回と同時に距離を変える。省略時は一定距離。
- `smooth_start_stop`: 角度と距離に5次補間を使う。既定はfalseで、既存sceneの動きは維持。

入力検証、sceneの保存・再読込、ハーネスのパラメーター情報に対応。到達位置、距離、端点付近の滑らかさ、不正な半径、scene往復保存をテスト。

## まだ不足している機能

| 優先度 | 不足 | 今回の対処と今後の必要性 |
|---|---|---|
| 高 | 全尺動画を評価する標準工程 | 外部スクリプトでフレーム欠落、明暗、隣接差分を検査。ハーネスへplayblast、問題区間の抽出、画像評価を統合したい。差分だけでは正常な移動とちらつきを分離できない。 |
| 高 | GPUプロセス間の排他と異常終了からの自動復旧 | 別GPU描画と重ねて起動したプレビューがexit 139で終了。単独実行へ切り替えた。同時実行との因果は未確定。プロセスをまたぐGPU排他、診断ログ、失敗フレーム再試行が必要。 |
| 高 | 時間方向のアンチエイリアス／シャッター時間 | 現状は1時刻の空間サンプルのみ。今回は緩やかな経路と高sppを使う。高速移動や強いハイライトにはモーションブラー、時間方向の評価が必要。 |
| 中 | 独立したフォーカス制御 | animationはfocus distanceを注視点までの距離に合わせる。今回は注視対象へ合焦させる構成とした。フォーカス送りや独立した焦点距離のkeyframeが欲しい。 |
| 中 | 色管理と出力プロファイル | 今回 `tools/encode-sdr.sh` でsRGB→BT.709の変換・limited range・色タグを明示する補助工程を追加。短い試作で動作確認済み。標準ハーネスへの型付き出力プロファイルの統合は未対応。 |
| 中 | HDR中間保存・独立したカラー調整 | 色・露出調整にもGPU再描画が必要。線形HDRを残し、仕上げ調整を独立させたい。 |
| 中 | サンプル途中のcheckpoint・適応サンプリング | 1枚が数十秒かかり、再開単位はフレーム。形状を変えずに反復計算を削減する仕組みが必要。 |
| 中 | 汎用カメラ経路 | 今回の追加で周回＋接近には対応。注視点が移るsplineや、速度・遮蔽を含む経路検証は未対応。 |

複数ショット編集、音楽・タイトルの合成、公開操作は今回の制作範囲に含めない。

## 再現

```bash
env WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- \
  preview scenes/production/ivory-vault.yaml --profile proof

env WGPU_BACKEND=gl cargo run --release -p fractal-renderer-cli -- \
  render scenes/production/ivory-vault.yaml \
  --output output/ivory-vault/reproduction-frames \
  --video-output output/ivory-vault/reproduction.mp4
```

長時間描画中は、同じGPUで別の描画プロセスを起動しない。

## SDR書き出し

`tools/encode-sdr.sh`は10-bit YUV中間を経由し、sRGBの伝達特性をBT.709へ変換して8-bit 4:2:0へ書き出す。既存出力を置換せず、一時ファイルの完成後に公開する。変換指定は[FFmpeg公式のcolorspace仕様](https://ffmpeg.org/ffmpeg-filters.html#colorspace)とインストール済みFFmpegのfilter helpで確認した。

```bash
bash tools/encode-sdr.sh FRAMES_DIRECTORY 30 360 output/ivory-vault/ivory-vault.mp4
```

色変換の往復検証では、sRGB PNG→BT.709 H.264→明示的な逆伝達変換→RGBのPSNRは38.69 dB。圧縮と4:2:0化の誤差を含む値であり、無劣化を意味しない。記録: `development/color-roundtrip.json`。
