//! Minimal stand-in for the parts of the Tauri API used by the desktop shell.
//! It exists only to type check the shell on a host without webkit and gtk.
//! The signatures mirror the ones of the real `tauri` crate.
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub trait Runtime: Send + Sync + 'static {}
impl Runtime for () {}

pub struct AppHandle<R: Runtime = ()> {
    state: Arc<Mutex<HashMap<TypeId, &'static (dyn Any + Send + Sync)>>>,
    events: Arc<Mutex<Vec<(String, String)>>>,
    trays: Arc<Mutex<HashMap<String, tray::TrayIcon<R>>>>,
    marker: PhantomData<R>,
}

impl<R: Runtime> Clone for AppHandle<R> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            events: Arc::clone(&self.events),
            trays: Arc::clone(&self.trays),
            marker: PhantomData,
        }
    }
}

impl<R: Runtime> Default for AppHandle<R> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(Vec::new())),
            trays: Arc::new(Mutex::new(HashMap::new())),
            marker: PhantomData,
        }
    }
}

impl<R: Runtime> AppHandle<R> {
    pub fn manage<T: Send + Sync + 'static>(&self, value: T) {
        let leaked: &'static T = Box::leak(Box::new(value));
        let erased: &'static (dyn Any + Send + Sync) = leaked;
        self.state
            .lock()
            .unwrap()
            .insert(TypeId::of::<T>(), erased);
    }

    pub fn emitted(&self) -> Vec<(String, String)> {
        self.events.lock().unwrap().clone()
    }

    pub fn exit(&self, _exit_code: i32) {}

    pub fn tray_by_id<'a, I>(&self, id: &'a I) -> Option<tray::TrayIcon<R>>
    where
        I: ?Sized,
        tray::TrayIconId: PartialEq<&'a I>,
    {
        self.trays.lock().unwrap().values().next().cloned()
    }
}

pub struct State<'r, T>(&'r T);

impl<T> std::ops::Deref for State<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

pub trait Manager<R: Runtime> {
    fn state<T: Send + Sync + 'static>(&self) -> State<'_, T>;
    fn get_webview_window(&self, label: &str) -> Option<webview::WebviewWindow<R>>;
}

impl<R: Runtime> Manager<R> for AppHandle<R> {
    fn state<T: Send + Sync + 'static>(&self) -> State<'_, T> {
        let guard = self.state.lock().unwrap();
        let erased: &'static (dyn Any + Send + Sync) =
            *guard.get(&TypeId::of::<T>()).expect("state not managed");
        let value: &'static T = erased.downcast_ref::<T>().expect("wrong state type");
        State(value)
    }

    fn get_webview_window(&self, label: &str) -> Option<webview::WebviewWindow<R>> {
        if label == "main" {
            Some(webview::WebviewWindow {
                marker: PhantomData,
            })
        } else {
            None
        }
    }
}

pub trait Emitter<R: Runtime> {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> crate::Result<()>;
}

impl<R: Runtime> Emitter<R> for AppHandle<R> {
    fn emit<S: serde::Serialize + Clone>(&self, event: &str, payload: S) -> crate::Result<()> {
        self.events
            .lock()
            .unwrap()
            .push((event.to_string(), serde_json::to_string(&payload).unwrap_or_default()));
        Ok(())
    }
}

pub mod async_runtime {
    pub fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(
        future: F,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(future)
    }
}

pub mod image {
    pub struct Image<'a> {
        pub bytes: &'a [u8],
    }

    impl<'a> Image<'a> {
        pub fn from_bytes(bytes: &'a [u8]) -> crate::Result<Self> {
            Ok(Self { bytes })
        }
    }
}

pub mod webview {
    use std::marker::PhantomData;

    use crate::Runtime;

    pub struct WebviewWindow<R: Runtime> {
        pub(crate) marker: PhantomData<R>,
    }

    impl<R: Runtime> WebviewWindow<R> {
        pub fn show(&self) -> crate::Result<()> {
            Ok(())
        }
        pub fn unminimize(&self) -> crate::Result<()> {
            Ok(())
        }
        pub fn set_focus(&self) -> crate::Result<()> {
            Ok(())
        }
        pub fn hide(&self) -> crate::Result<()> {
            Ok(())
        }
    }
}

pub mod menu {
    use crate::{AppHandle, Manager, Runtime};

    pub trait ContextMenu {}

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct MenuId(pub String);

    impl From<&str> for MenuId {
        fn from(value: &str) -> Self {
            Self(value.to_string())
        }
    }

    pub trait IsMenuItem<R: Runtime> {}

    pub struct MenuItem<R: Runtime> {
        id: MenuId,
        text: String,
        enabled: bool,
        marker: std::marker::PhantomData<R>,
    }

    impl<R: Runtime> IsMenuItem<R> for MenuItem<R> {}

    impl<R: Runtime> MenuItem<R> {
        pub fn with_id<M, I, T, A>(
            manager: &M,
            id: I,
            text: T,
            enabled: bool,
            _accelerator: Option<A>,
        ) -> crate::Result<Self>
        where
            M: Manager<R>,
            I: Into<MenuId>,
            T: AsRef<str>,
            A: AsRef<str>,
        {
            let _ = manager;
            Ok(Self {
                id: id.into(),
                text: text.as_ref().to_string(),
                enabled,
                marker: std::marker::PhantomData,
            })
        }
    }

    pub struct Menu<R: Runtime> {
        marker: std::marker::PhantomData<R>,
    }

    impl<R: Runtime> ContextMenu for Menu<R> {}

    impl<R: Runtime> Menu<R> {
        pub fn with_items<M: Manager<R>>(manager: &M, _items: &[&dyn IsMenuItem<R>]) -> crate::Result<Self> {
            let _ = manager;
            Ok(Self {
                marker: std::marker::PhantomData,
            })
        }
    }

    pub struct MenuEvent {
        pub id: MenuId,
    }

    impl<R: Runtime> AppHandle<R> {
        pub fn menu(&self) -> Option<&Menu<R>> {
            None
        }
    }
}

pub mod tray {
    use crate::menu::{ContextMenu, Menu, MenuId};
    use crate::{AppHandle, Manager, Runtime};

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    pub struct TrayIconId(pub String);

    impl From<&str> for TrayIconId {
        fn from(value: &str) -> Self {
            Self(value.to_string())
        }
    }

    impl PartialEq<&str> for TrayIconId {
        fn eq(&self, other: &&str) -> bool {
            self.0 == *other
        }
    }

    impl PartialEq<&str> for &TrayIconId {
        fn eq(&self, other: &&str) -> bool {
            self.0 == *other
        }
    }

    pub struct TrayIcon<R: Runtime> {
        marker: std::marker::PhantomData<R>,
    }

    impl<R: Runtime> Clone for TrayIcon<R> {
        fn clone(&self) -> Self {
            Self {
                marker: std::marker::PhantomData,
            }
        }
    }

    impl<R: Runtime> TrayIcon<R> {
        pub fn set_menu<M: ContextMenu + 'static>(&self, _menu: Option<M>) -> crate::Result<()> {
            Ok(())
        }
        pub fn set_tooltip<S: AsRef<str>>(&self, _tooltip: Option<S>) -> crate::Result<()> {
            Ok(())
        }
    }

    pub struct TrayIconBuilder<R: Runtime> {
        marker: std::marker::PhantomData<R>,
    }

    impl<R: Runtime> TrayIconBuilder<R> {
        pub fn with_id<I: Into<TrayIconId>>(_id: I) -> Self {
            Self {
                marker: std::marker::PhantomData,
            }
        }
        pub fn menu<M: ContextMenu>(mut self, _menu: &M) -> Self {
            self
        }
        pub fn icon(mut self, _icon: crate::image::Image<'_>) -> Self {
            self
        }
        pub fn tooltip<S: AsRef<str>>(mut self, _s: S) -> Self {
            self
        }
        pub fn show_menu_on_left_click(mut self, _enable: bool) -> Self {
            self
        }
        pub fn on_menu_event<F: Fn(&AppHandle<R>, crate::menu::MenuEvent) + Sync + Send + 'static>(
            mut self,
            _f: F,
        ) -> Self {
            self
        }
        pub fn build<M: Manager<R>>(self, _manager: &M) -> crate::Result<TrayIcon<R>> {
            Ok(TrayIcon {
                marker: std::marker::PhantomData,
            })
        }
    }
}

pub use tauri_macros_stub::command;
