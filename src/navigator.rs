//! Minimal `NavigatorInterface` for the desktop app.

use std::fs::File;
use std::io;
use std::path::Path;

use ruffle_frontend_utils::backends::navigator::NavigatorInterface;
use url::Url;

use crate::log::{self, SharedLogState};

#[derive(Clone)]
pub struct DesktopNavigatorInterface {
    pub log_state: SharedLogState,
}

impl NavigatorInterface for DesktopNavigatorInterface {
    fn navigate_to_website(&self, url: Url) {
        let url_str = url.as_str();
        tracing::debug!("navigate_to_website: {}", url_str);

        if url_str.starts_with("javascript:")
            && log::handle_javascript_url(url_str, &self.log_state)
        {
            return;
        }

        if let Err(error) = webbrowser::open(url_str) {
            tracing::error!("Could not open URL {}: {error}", url);
        }
    }

    async fn open_file(&self, _path: &Path) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file access is not supported by this launcher",
        ))
    }

    async fn confirm_socket(&self, _host: &str, _port: u16) -> bool {
        true
    }

    fn is_javascript_url_allowed(&self, url: &Url) -> bool {
        let allowed = log::is_log_javascript_url(url.as_str());
        tracing::debug!("is_javascript_url_allowed: {} = {}", url.as_str(), allowed);
        allowed
    }
}
