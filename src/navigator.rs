//! Minimal `NavigatorInterface` for the desktop app.

use std::fs::File;
use std::io;
use std::path::Path;

use ruffle_frontend_utils::backends::navigator::NavigatorInterface;
use url::Url;

#[derive(Clone)]
pub struct MinimalNavigatorInterface;

impl NavigatorInterface for MinimalNavigatorInterface {
    fn navigate_to_website(&self, url: Url) {
        if let Err(error) = webbrowser::open(url.as_str()) {
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
}
