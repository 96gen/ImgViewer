# ImgViewer verification guide

以下命令均從 repository root 執行。自動測試與人工 packaged smoke 是兩個獨立 gate；兩者都通過才可發布 ZIP。

## 1. 靜態、單元與整合測試

```powershell
pnpm install --frozen-lockfile
pnpm test
pnpm build
cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path .\src-tauri\Cargo.toml --all-targets --no-default-features -- -D warnings
cargo test --manifest-path .\src-tauri\Cargo.toml --no-default-features
```

PASS anchor：每個命令 exit code 都是 0；Vitest 不得有 leaked Blob URL 測試失敗；Rust 競態測試必須確認最後 generation 的像素與名稱一致，不只是索引一致。16-bit PNG 測試必須證明先做色彩轉換、最後才量化成帶 ICC 的 RGBA8；Display P3 測試必須符合獨立 sRGB 參考值；CICP 與非 RGB ICC 必須走明確的轉換或可恢復錯誤，而等效 sRGB ICC 的普通圖片仍保留原始 bytes。

## 2. 原生 Windows smoke（無 WDIO／WebDriver）

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -KeepStage -SkipNativeSmoke
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-native.ps1 -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

腳本直接啟動正式 binary，以 Windows UI Automation 確認 `<img>` 已實際出現在 WebView2 accessibility tree，並由 Win32 `GetWindowRect` 比對窗口幾何；不注入 JavaScript、不開測試 IPC，也不安裝 WDIO、WebDriver 或測試版 Tauri plugin。執行前應關閉其他 ImgViewer 實例。

`build-portable.ps1` 預設會自行執行同一個 smoke；此處加入 `-SkipNativeSmoke`，是為了把建置與 smoke 的 PASS 輸出分開展示，避免重複執行。無互動式桌面的 CI 才應單獨略過此 gate。

完整像素模式的 PASS anchor：輸出 `PASS switch-continuity ... uia-image-min=1 pixel=old-or-new ... webdriver=absent` 及 `PASS native-smoke formats=7 animations=2 navigation=4 continuity=1 error-recovery=1 webdriver=absent`。六種格式與 `.heif` 都須建立實際 `<img>`；GIF/WebP 各取樣到至少兩種 frame color；`1/2/10.jpg` 的中心像素須分別呈紅／綠／藍主色，快速切圖最後必須是藍色 `10.jpg`，不能只靠名稱。純紅 PNG 切到大型純綠 TIFF 時，每個 10 ms 樣本都至少要有一個 UIA Image，中心像素只能是舊紅或新綠，不可出現背景色／spinner；損壞檔後須能以方向鍵恢復。每項原生 window rect 都必須與基準完全相同。

使用 `-SkipPixelChecks` 時只接受 `PASS switch-continuity ... uia-image-min=1 pixel=skipped ...` 及 `PASS native-ui-smoke formats=7 ... continuity=1 ... pixel-checks=skipped ...`。這代表切換期間 UIA 圖片元素不中斷、縮放控制與窗口 rect 已驗證，但不代表 framebuffer、動畫換幀或顏色連續性已通過；不可拿它取代有互動式桌面的完整 release gate。

若遠端桌面不接受模擬鍵盤／滑鼠，只執行正式 single-instance 交接的無空白專項 gate：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-native.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe" `
  -ContinuityOnly -SkipPixelChecks
```

此模式仍用 packaged EXE、24MP TIFF 與 10 ms UIA 採樣，不注入前端；但它只證明切換期間圖片元素不中斷及窗口 rect 不變，不能替代完整格式、鍵盤、縮放與 framebuffer gate。

同一類無互動桌面可另跑七格式 single-instance handoff gate：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-native.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe" `
  -HandoffFormatsOnly -SkipPixelChecks
```

它應輸出 `PASS handoff-format-smoke formats=7 animations-opened=2 rect=unchanged webdriver=absent`；此處的 `animations-opened` 只代表 animated GIF/WebP 成功建立圖片元素，不代表 framebuffer 換幀已驗證。

### RAM／效能 smoke（無 WDIO／WebDriver）

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-memory.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

腳本會循環以第二實例交接三張動態產生的 PNG，UI Automation 確認目標 `<img>` 後，遞迴合計主程式與全部 `msedgewebview2.exe` 子程序。CSV 包含每輪 load time、切圖期間 peak private/working set、固定 idle 後 retained private/working set。PASS anchor 為 `PASS memory-smoke ... webdriver=absent`；預設 gate 另檢查 retained growth、線性斜率與 p95 load。RAM 絕對值受 WebView2 版本、GPU、DPI 與螢幕影響，before／after 比較必須固定環境與參數。

## 3. Portable release build

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -KeepStage
```

PASS anchor：

- `release/ImgViewer-{version}-windows-x64.zip` 存在。
- release gate 的 Clippy 與 Rust tests 都以 `--features heic` 通過，確實編譯並執行 native HEIC 路徑。
- stage 內至少包含 `ImgViewer.exe`、`heif.dll`、`libde265.dll`、必要 `vcruntime*.dll` / `msvcp*.dll`、`README.md`、`LICENSE`、`THIRD_PARTY_NOTICES.md`、`SOURCE_VERSIONS.txt` 與 `licenses/`。
- 腳本最後一次 dependency scan 顯示零個 unresolved non-system DLL。
- `SOURCE_VERSIONS.txt` 顯示 vcpkg commit `d015e31e90838a4c9dfa3eed45979bc70d9357fc`、libheif 1.21.2 與 libde265 1.0.18。
- stage 不含 `x265.dll`、AOM/AVIF codec 或 WebView2 offline installer。

## 4. Packaged smoke matrix

只測 `release` ZIP 解壓後的內容，不使用 `pnpm tauri dev`。先記錄窗口 `outerPosition` 與 `outerSize`，逐項切換後再讀取並比較原始數值。

| Case | 操作 | PASS anchor |
| --- | --- | --- |
| 圖片格式 | 依序開啟 JPG、PNG、GIF、TIFF、WebP、HEIC/HEIF | 每張像素/透明度正確；TIFF 是第一頁；HEIC 是 primary image |
| 動畫 | 停留在 GIF 與 animated WebP 至少兩個 frame duration | 兩者都可觀察到換幀 |
| 尺寸不變 | 橫圖、直圖、1×1、TIFF、HEIC、動畫互切 | 每次原生 `outerPosition` / `outerSize` 與基準完全相同 |
| 快速切圖 | 在 `1.jpg, 2.jpg, 10.jpg` 間連按方向鍵 | 最後名稱、索引與實際像素都是最後目標；無舊 generation 閃回 |
| 首尾與錯誤 | 到首尾再按；刪除中間檔；開啟 corrupt / disguised / oversize fixture | 不循環；顯示可恢復內嵌錯誤；仍可切到下一張 |
| 縮放平移 | 滾輪、拖曳、`Ctrl+0`、`Ctrl+1` | 10%–1600% clamp；Fit 不裁切且小圖不超過 100% |
| 開啟入口 | 按鈕、`Ctrl+O`、拖放、命令列路徑 | 四種入口都開啟同一檔案 |
| single instance | 保持第一個窗口，再執行 `ImgViewer.exe <path>` | 既有窗口開圖並聚焦；沒有第二個 viewer 窗口 |
| 狀態還原 | 改位置/尺寸/最大化後關閉重啟 | 還原狀態；載圖不改 bounds |
| 工作區校正 | 拔除副螢幕或改 DPI 後啟動 | 窗口完整落在現有螢幕可見工作區 |

## 5. Clean VM gate

各在 Windows 10 22H2 x64 與 Windows 11 x64 的乾淨 VM 執行：

1. 確認未安裝 Windows HEIF Image Extensions。
2. 只複製 release ZIP，解壓並執行。
3. 開啟 `primary-second.heic` 與一般 HEIC 相片。
4. 若 VM 刻意移除 WebView2，應得到缺少 runtime 的明確系統錯誤；從 Microsoft Evergreen 頁安裝後應可啟動。ZIP 本身不應含約 127 MiB 的 offline WebView2 installer。

PASS anchor：兩台 VM 都不需安裝 ImgViewer 或 HEIF Extension即可解碼 HEIC；Process Monitor 不應顯示從開發機路徑載入 DLL。

## 6. Expected failures

- 暫時從解壓資料夾移走 `libde265.dll`：HEIC 路徑必須失敗，證明 VM 沒有偷偷使用 HEIF Extension；還原 DLL 後恢復。
- 暫時從 stage 移走非系統 DLL，再執行：

  ```powershell
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Resolve-NativeDependencies.ps1 -StageDirectory .\release\ImgViewer-{version}-windows-x64 -RequireBundledMsvcRuntime
  ```

  dependency scan 必須以 unresolved DLL 非零結束，不得誤報 PASS。
- 將 PNG 改名成 `.jpg`：應回報格式偽裝，不得依副檔名直接顯示。
