# 安全政策

## 支援範圍

只有最新 stable 版本會收到安全修補。舊版本與開發中版本僅提供
best-effort 協助，不承諾回補。

ImgViewer 是 Windows、離線、唯讀、圖片專用程式；沒有 PDF、網路、
遙測、自動更新、shell 或一般檔案系統 API。

## 私下回報漏洞

請使用 GitHub repository 的 **Security → Advisories → Report a
vulnerability** 私下回報。維護者必須在 repository 設定中啟用
Private vulnerability reporting。

若該按鈕暫時不可用，請勿把漏洞細節、exploit、crash dump 或圖片貼到
公開 issue；可先開一則不含技術細節的 issue，請維護者提供私人聯絡方式。

回報請盡量包含：

- 受影響的 ImgViewer 版本與 Windows 版本。
- 可重現步驟、預期結果與實際結果。
- 經去識別化的最小 fixture，或 fixture 的雜湊與產生方法。
- 已移除使用者名稱、完整路徑、記憶體位址及圖片內容的記錄。
- 已知的影響、前置條件與可能的緩解方式。

請勿提供私人照片。若 reproducer 必須包含敏感內容，請先只描述問題，
待維護者建立合適的私人交換方式。

## 處理目標

以下是維護者的內部目標，不是商業 SLA：

- **Critical**：可藉開圖執行程式、任意寫檔、逃離安全邊界，或已有實際
  利用的可達依賴漏洞。收到當天先標示影響，72 小時內以修補、停用格式
  或撤下下載方式止血。
- **High**：容易觸發的永久 DoS、重大隱私洩漏或權限繞過，目標 14 天內
  修正。
- **Medium / Low**：排入下一次季度維護。

若短期內無法安全修復，維護者會公開受影響範圍並暫停散布，或先停用
相關格式。修復與使用者有可行緩解方式後，才協調公開完整細節。

## 安全設計與限制

安全邊界與目前尚未完成的隔離工作記錄於
[THREAT_MODEL.md](THREAT_MODEL.md)。任何 Capability、CSP、Tauri plugin、
原生 codec 或輸入限制變更，都必須在 PR 中附上威脅分析與負向測試。

TIFF 與 HEIC／HEIF 解碼在私有 helper process 中執行。helper 只接受 broker
duplicated 的 read-only handle，受 768 MiB、單一 process、kill-on-close
的 Windows Job Object 與每張 30 秒硬期限約束；timeout、crash 或 protocol
錯誤後不會自動重試同一輸入。回傳 RGBA8 的 safe Rust PNG 編碼會逐列檢查
取消與剩餘期限，過期或 partial 結果不發布。WebView2 動畫解碼尚未具備同等硬隔離，
仍是明確記錄的 DoS 殘餘風險。
