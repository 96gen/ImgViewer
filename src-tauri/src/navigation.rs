use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::{Runtime, Url};

pub(crate) fn navigation_policy_plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("navigation-policy")
        .on_navigation(|webview, url| {
            webview.label() == "main"
                && is_allowed_top_level_navigation(url, cfg!(debug_assertions))
        })
        .build()
}

fn is_allowed_top_level_navigation(url: &Url, allow_dev_server: bool) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }

    // tauri.conf does not enable useHttpsScheme, so Wry registers only this
    // exact HTTP custom-protocol origin. Allowing HTTPS as well could load a
    // non-app loopback endpoint into the capability-bearing main window.
    let production_origin =
        url.scheme() == "http" && url.host_str() == Some("tauri.localhost") && url.port().is_none();
    let development_origin = allow_dev_server
        && url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == Some(1420);

    production_origin || development_origin
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).expect("test URL should parse")
    }

    #[test]
    fn production_allows_only_the_embedded_tauri_origin() {
        assert!(is_allowed_top_level_navigation(
            &url("http://tauri.localhost/"),
            false
        ));
        for denied in [
            "https://example.com/",
            "https://tauri.localhost/",
            "http://tauri.localhost:8080/",
            "http://attacker@tauri.localhost/",
            "file:///C:/private/photo.jpg",
            "data:text/html,untrusted",
            "blob:http://tauri.localhost/00000000-0000-0000-0000-000000000000",
            "tauri://localhost/",
        ] {
            assert!(
                !is_allowed_top_level_navigation(&url(denied), false),
                "production navigation unexpectedly allowed {denied}"
            );
        }
    }

    #[test]
    fn development_allows_only_the_configured_loopback_origin() {
        assert!(is_allowed_top_level_navigation(
            &url("http://127.0.0.1:1420/"),
            true
        ));
        assert!(is_allowed_top_level_navigation(
            &url("http://127.0.0.1:1420/src/main.ts"),
            true
        ));

        for denied in [
            "http://localhost:1420/",
            "http://127.0.0.1/",
            "http://127.0.0.1:1421/",
            "https://127.0.0.1:1420/",
            "http://127.0.0.2:1420/",
        ] {
            assert!(
                !is_allowed_top_level_navigation(&url(denied), true),
                "development navigation unexpectedly allowed {denied}"
            );
        }
    }
}
