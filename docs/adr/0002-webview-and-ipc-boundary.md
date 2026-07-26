# ADR 0002：WebView 與 IPC 採最小權限

- 狀態：Accepted
- 日期：2026-07-23

## 決策

Vue WebView 只負責顯示與輸入，不是檔案存取的信任邊界。只有 `main`
窗口可呼叫固定 viewer commands、開啟 dialog 與監聽事件；圖片使用一次性
binary render token，不啟用 HTTP、shell、opener、一般 filesystem 或
global Tauri API。

production CSP 僅允許本地資源、Tauri IPC 與 `blob:` 圖片。Capability、
CSP、plugin 或 command surface 的擴張一律視為安全性變更。

## 影響

選檔與拖放仍會把使用者選擇的路徑交給 UI，因此 Rust 必須自行驗證所有
輸入，且 snapshot、錯誤和診斷不能回傳完整路徑。相關變更必須附負向測試。
