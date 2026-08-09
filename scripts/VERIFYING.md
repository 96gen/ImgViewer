# ImgViewer verification guide

以下命令均從 repository root 執行。自動測試與人工 packaged smoke 是兩個獨立 gate；兩者都通過才可發布 ZIP。

## 1. 靜態、單元與整合測試

```powershell
pnpm install --frozen-lockfile
pnpm test
pnpm build
cargo fmt --manifest-path .\src-tauri\Cargo.toml --all -- --check
cargo clippy --locked --manifest-path .\src-tauri\Cargo.toml --workspace --all-targets --no-default-features -- -D warnings
cargo test --locked --manifest-path .\src-tauri\Cargo.toml --workspace --no-default-features
cargo clippy --locked --manifest-path .\src-tauri\Cargo.toml --package imgviewer-codec-core --all-targets --no-default-features --features heic,tiff -- -D warnings
cargo test --locked --manifest-path .\src-tauri\Cargo.toml --package imgviewer-codec-core --no-default-features --features heic,tiff
cargo clippy --locked --manifest-path .\src-tauri\Cargo.toml --package imgviewer-codec-helper --all-targets --no-default-features --features heic,tiff -- -D warnings
cargo test --locked --manifest-path .\src-tauri\Cargo.toml --package imgviewer-codec-helper --no-default-features --features heic,tiff
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Assert-CargoFeatureBoundary.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-codec-helper-process.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\test-release-contract.ps1
```

PASS anchor：每個命令 exit code 都是 0；helper process gate必須輸出
`PASS codec-helper-process formats=heif,tiff persistent=1 crash-restarts=20 hang-recovery=1 oom-recovery=1 handle-release=verified orphan=absent`。
Vitest 不得有 leaked Blob URL 測試失敗；Rust 競態測試必須確認最後
generation 的像素與名稱一致，不只是索引一致。16-bit PNG 測試必須證明先做
色彩轉換、最後才量化成帶 ICC 的 RGBA8；Display P3 測試必須符合獨立 sRGB
參考值；CICP 與非 RGB ICC 必須走明確的轉換或可恢復錯誤，而等效 sRGB ICC
的普通圖片仍保留原始 bytes。

## 2. 原生 Windows smoke（無 WDIO／WebDriver）

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -KeepStage -SkipNativeSmoke
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-native.ps1 -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"
```

腳本直接啟動正式 binary，以 Windows UI Automation 確認 `<img>` 已實際出現在 WebView2 accessibility tree，並由 Win32 `GetWindowRect` 比對窗口幾何；不注入 JavaScript、不開測試 IPC，也不安裝 WDIO、WebDriver 或測試版 Tauri plugin。執行前應關閉其他 ImgViewer 實例。

`build-portable.ps1` 預設會自行執行同一個 smoke；此處加入 `-SkipNativeSmoke`，是為了把建置與 smoke 的 PASS 輸出分開展示，避免重複執行。無互動式桌面的 CI 才應單獨略過此 gate。

完整像素模式的 PASS anchor：輸出 `PASS switch-continuity ... uia-image-min=1 pixel=old-or-new ... webdriver=absent`、`PASS native-smoke formats=7 animations=2 navigation=4 continuity=1 error-recovery=1 webdriver=absent` 及關閉主程式後才產生的 `PASS codec-helper-runtime sibling=verified direct-child=1 persistent-pid=... orphan=absent webdriver=absent`。六種格式與 `.heif` 都須建立實際 `<img>`；TIFF、HEIC 到 HEIF 解碼必須沿用同一個唯一 direct-child helper PID；GIF/WebP 各取樣到至少兩種 frame color；`1/2/10.jpg` 的中心像素須分別呈紅／綠／藍主色。單張自然排序仍用 Arrow key 驗證鍵盤路徑；`PASS rapid-navigation ... trigger=uia-burst count=2 ...` 則用同一 UIA 按鈕無等待連按兩次，避免把 `SendKeys` 漏鍵誤判成解碼競態，最後仍必須是藍色 `10.jpg`，不能只靠名稱。`Wait-Image` 超時會列出當時的 UIA Image、文字與最後例外，供區分輸入、狀態及 render 問題。純紅 PNG 切到大型純綠 TIFF 時，每個 10 ms 樣本都至少要有一個 UIA Image，中心像素只能是舊紅或新綠，不可出現背景色／spinner；損壞檔後須能以方向鍵恢復。每項原生 window rect 都必須與基準完全相同。

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

它應輸出 `PASS handoff-format-smoke formats=7 animations-opened=2 rect=unchanged webdriver=absent` 與 `PASS codec-helper-runtime ... orphan=absent ...`；此處的 `animations-opened` 只代表 animated GIF/WebP 成功建立圖片元素，不代表 framebuffer 換幀已驗證。

### RAM／效能 smoke（無 WDIO／WebDriver）

```powershell
$version = (Get-Content .\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-memory.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe"

# 0.4 codec isolation gate：TIFF／HEIF 每輪交替，共用同一 helper PID。
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-memory.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe" `
  -IsolatedCodecAlternation -Cycles 100 -Warmup 70
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\smoke-memory.ps1 `
  -Executable ".\release\ImgViewer-$version-windows-x64\ImgViewer.exe" `
  -IsolatedCodecAlternation -Cycles 160 -Warmup 110
```

預設模式先以固定 HEIC fixture 啟動唯一 direct-child helper，再循環以第二實例
交接三張動態產生的 PNG；`-IsolatedCodecAlternation` 則從 TIFF 啟動並逐輪
交替 TIFF／HEIF，要求 helper PID 全程不變。UI Automation 確認目標 `<img>`
後，總量會分別加總主程式、全部 `msedgewebview2.exe` 與
`ImgViewer.CodecHelper.exe`；CSV 另列 helper PID、private/working set 與 peak。
PASS anchor 分別含 `helper-source=heic` 或 `helper-source=tiff-heif`，關閉主程式後
還必須輸出 `PASS memory-helper-cleanup ... orphan=absent ...`。預設 gate另檢查
包含 helper 的 retained growth、線性斜率與 p95 load。RAM 絕對值受 WebView2
版本、GPU、DPI 與螢幕影響，before／after比較必須固定環境與參數。

## 3. Portable release build

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-portable.ps1 -KeepStage
```

PASS anchor：

- `release/ImgViewer-{version}-windows-x64.zip` 存在。
- release gate 先對全 workspace 跑無隔離 codec feature 的 Clippy/tests，再對 codec core 與 helper 以 `--features heic,tiff` 編譯及測試 HEIF/TIFF 路徑；Tauri 主程式本身不得啟用這兩個 feature。
- Cargo feature graph 必須顯示主程式沒有 `image/tiff`、`tiff` 或 libheif，而 helper 同時有 TIFF 與 libheif；這項檢查獨立於 PE import gate。
  Exact PASS anchor：`PASS codec-feature-boundary main-heif=absent main-tiff=absent helper-heif=present helper-tiff=present`。
- stage 內至少包含 `ImgViewer.exe`、`ImgViewer.CodecHelper.exe`、`heif.dll`、`libde265.dll`、必要 `vcruntime*.dll` / `msvcp*.dll`、`README.md`、`LICENSE`、`THIRD_PARTY_NOTICES.md`、`SOURCE_VERSIONS.txt` 與 `licenses/`。
- 腳本最後一次 dependency scan 顯示零個 unresolved non-system DLL。
- `dumpbin /DEPENDENTS` 必須證明主程式不匯入 `heif.dll`／`libde265.dll`，且 helper 直接匯入 `heif.dll`。
- `BUILD_METADATA.json` schema v3 必須包含 main/helper 的 role、SHA-256、protocol v3，以及 `heif,tiff` isolation formats、`heic,tiff` Cargo features、805306368-byte memory limit 與 30000-ms decode deadline；下載後驗證須拒絕 helper 遺失或 hash 不符。
- CycloneDX SBOM 必須包含四個 Rust workspace crate、Cargo `tiff` component、helper payload hash、六個 isolation properties、libheif、libde265 與 MSVC runtime。
- `SOURCE_VERSIONS.txt` 顯示 vcpkg commit `d015e31e90838a4c9dfa3eed45979bc70d9357fc`、libheif 1.21.2 與 libde265 1.0.18。
- stage 不含 `x265.dll`、AOM/AVIF codec、fault-helper、test-hooks 產物或 WebView2 offline installer。

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

## 5. Maintained-host release gate

VM 永久不列入 release、年度維護或 1.0 gate。正式 tag workflow 建立 draft
後，下載同一份 hosted ZIP 與 checksum，在目前維護的互動式 Windows 11
實機核對 digest，解壓後執行第 2、4 節的 packaged smoke、RAM 與 helper
orphan gate。Windows 10 維持 best effort，不阻擋安全更新或發布。

PASS anchor：接受測試的 ZIP SHA-256 必須與 draft asset、checksum manifest
及 promotion input 完全一致；stage 只能從解壓後目錄載入 bundled helper 與
native DLL，且 ZIP 不包含 WebView2 offline installer。

## 6. Expected failures

- 暫時從解壓資料夾移走 `libde265.dll`：dependency closure／portable verifier
  必須拒絕，HEIC 路徑不得改走系統 extension 或其他 fallback；還原 bundled
  DLL 後恢復。
- 暫時從 stage 移走非系統 DLL，再執行：

  ```powershell
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Resolve-NativeDependencies.ps1 -StageDirectory .\release\ImgViewer-{version}-windows-x64 -RequireBundledMsvcRuntime
  ```

  dependency scan 必須以 unresolved DLL 非零結束，不得誤報 PASS。
- 將 PNG 改名成 `.jpg`：應回報格式偽裝，不得依副檔名直接顯示。
- 暫時移走 `ImgViewer.CodecHelper.exe`，或修改其任一 byte：`verify-portable-release.ps1` 必須分別以 missing-helper 或 helper-hash-mismatch 拒絕。
- 將 `BUILD_METADATA.json` 的 `native.platformToolset` 改成 `v145`：
  `verify-portable-release.ps1` 與 SBOM 產生器都必須拒絕，不能把 runner
  自動選到的新 toolset 當成已審核的 VC143 產物。
