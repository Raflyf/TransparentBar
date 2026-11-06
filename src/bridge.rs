#![allow(non_snake_case)]
use std::ffi::c_void;
use windows::core::{interface, IUnknown, IUnknown_Vtbl, HRESULT};
mod protocol;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SourceInfo {
    pub file: *mut u16,
    pub line: u32,
    pub column: u32,
    pub position: u32,
    pub hash: *mut u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Relation {
    pub parent: u64,
    pub child: u64,
    pub index: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VisualElement {
    pub handle: u64,
    pub source: SourceInfo,
    pub kind: *mut u16,
    pub name: *mut u16,
    pub children: u32,
}

#[interface("AA7A8931-80E4-4FEC-8F3B-553F87B4966E")]
unsafe trait ITreeCallback: IUnknown {
    fn OnVisualTreeChange(
        &self,
        relation: Relation,
        element: VisualElement,
        mutation: i32,
    ) -> HRESULT;
}
#[interface("A593B11A-D17F-48BB-8F66-83910731C8A5")]
unsafe trait ITreeService: IUnknown {
    fn AdviseVisualTreeChange(&self, callback: *mut c_void) -> HRESULT;
    fn UnadviseVisualTreeChange(&self, callback: *mut c_void) -> HRESULT;
}
#[interface("18C9E2B6-3F43-4116-9F2B-FF935D7770D2")]
unsafe trait IXamlDiagnostics: IUnknown {
    fn GetDispatcher(&self, out: *mut *mut c_void) -> HRESULT;
    fn GetUiLayer(&self, out: *mut *mut c_void) -> HRESULT;
    fn GetApplication(&self, out: *mut *mut c_void) -> HRESULT;
    fn GetIInspectableFromHandle(&self, handle: u64, out: *mut *mut c_void) -> HRESULT;
    fn GetHandleFromIInspectable(&self, object: *mut c_void, out: *mut u64) -> HRESULT;
    fn HitTest(
        &self,
        rect: windows::Win32::Foundation::RECT,
        count: *mut u32,
        handles: *mut *mut u64,
    ) -> HRESULT;
    fn RegisterInstance(&self, object: *mut c_void, out: *mut u64) -> HRESULT;
    fn GetInitializationData(&self, out: *mut *mut u16) -> HRESULT;
}
#[repr(transparent)]
#[derive(Clone)]
struct IUIElement(IUnknown);
unsafe impl windows::core::Interface for IUIElement {
    type Vtable = IUIElement_Vtbl;
    const IID: windows::core::GUID =
        windows::core::GUID::from_u128(0x676d0be9_b65c_41c6_ba40_58cf87f201c1);
}
#[repr(C)]
struct IUIElement_Vtbl {
    base: windows::core::IInspectable_Vtbl,
    desired_size: unsafe extern "system" fn(*mut c_void, *mut windows::Foundation::Size) -> HRESULT,
    get_allow_drop: unsafe extern "system" fn(*mut c_void, *mut bool) -> HRESULT,
    set_allow_drop: unsafe extern "system" fn(*mut c_void, bool) -> HRESULT,
    GetOpacity: unsafe extern "system" fn(*mut c_void, *mut f64) -> HRESULT,
    SetOpacity: unsafe extern "system" fn(*mut c_void, f64) -> HRESULT,
}

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use windows::{
    core::{implement, AgileReference, Error, Interface, Result, BSTR, GUID, PCWSTR},
    Win32::{
        Foundation::*,
        System::{Com::*, Ole::*, Threading::*},
    },
    UI::Core::{CoreDispatcher, CoreDispatcherPriority, DispatchedHandler},
};

#[derive(Default)]
struct TreeState {
    nodes: HashMap<u64, u64>,
    frames: HashSet<u64>,
    saved: HashMap<u64, (IUIElement, f64)>,
    targets: HashMap<u64, (u64, bool)>,
}
thread_local! { static TREE: RefCell<TreeState> = RefCell::new(TreeState::default()); }
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
struct Session {
    diagnostics: AgileReference<IXamlDiagnostics>,
    dispatcher: CoreDispatcher,
    active: Arc<AtomicBool>,
    ready: Arc<OwnedHandle>,
    failed: Arc<OwnedHandle>,
    done: Arc<OwnedHandle>,
}
struct OwnedHandle(usize);
impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        HANDLE(self.0 as *mut c_void)
    }
}
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.raw());
        }
    }
}

fn ancestor_frame(state: &TreeState, mut handle: u64) -> Option<u64> {
    for _ in 0..64 {
        if state.frames.contains(&handle) {
            return Some(handle);
        }
        let parent = *state.nodes.get(&handle)?;
        if parent == handle {
            return None;
        }
        handle = parent;
    }
    None
}

fn bstr_text(raw: *mut u16) -> String {
    if raw.is_null() {
        return String::new();
    }
    unsafe {
        let raw_ref = std::mem::ManuallyDrop::new(BSTR::from_raw(raw));
        raw_ref.to_string()
    }
}

fn queue(session: &Session, action: impl Fn() -> Result<()> + Send + 'static) -> Result<()> {
    let failed = session.failed.clone();
    session.dispatcher.RunAsync(
        CoreDispatcherPriority::Normal,
        &DispatchedHandler::new(move || {
            let result = action();
            if result.is_err() {
                unsafe {
                    let _ = SetEvent(failed.raw());
                }
            }
            result
        }),
    )?;
    Ok(())
}

impl Session {
    fn mutation(
        &self,
        relation: Relation,
        handle: u64,
        kind: String,
        name: String,
        mutation: i32,
    ) -> Result<()> {
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        let this = self.clone();
        let action = move || {
            if !this.active.load(Ordering::Acquire) {
                return Ok(());
            }
            let diagnostics = this.diagnostics.resolve()?;
            TREE.with(|tree| -> Result<()> {
                if mutation != 0 {
                    let saved = tree.borrow().saved.get(&handle).cloned();
                    if let Some((element, opacity)) = saved {
                        unsafe {
                            (element.vtable().SetOpacity)(element.as_raw(), opacity).ok()?;
                        }
                        tree.borrow_mut().saved.remove(&handle);
                    }
                    let mut state = tree.borrow_mut();
                    state.nodes.remove(&handle);
                    state.frames.remove(&handle);
                    state.targets.remove(&handle);
                    let complete = state.frames.iter().any(|frame| {
                        state
                            .targets
                            .values()
                            .any(|(owner, fill)| owner == frame && *fill)
                            && state
                                .targets
                                .values()
                                .any(|(owner, fill)| owner == frame && !*fill)
                    });
                    if !complete {
                        unsafe {
                            ResetEvent(this.ready.raw())?;
                        }
                    }
                    return Ok(());
                }
                let frame = {
                    let mut state = tree.borrow_mut();
                    state.nodes.insert(handle, relation.parent);
                    if kind == "Taskbar.TaskbarFrame" {
                        state.frames.insert(handle);
                    }
                    if kind != "Windows.UI.Xaml.Shapes.Rectangle"
                        || !matches!(name.as_str(), "BackgroundFill" | "BackgroundStroke")
                        || state.saved.contains_key(&handle)
                    {
                        return Ok(());
                    }
                    let Some(frame) = ancestor_frame(&state, relation.parent) else {
                        return Ok(());
                    };
                    frame
                };
                let mut raw = std::ptr::null_mut();
                unsafe {
                    diagnostics
                        .GetIInspectableFromHandle(handle, &mut raw)
                        .ok()?;
                }
                if raw.is_null() {
                    return Err(Error::from(E_POINTER));
                }
                let object = unsafe { IUnknown::from_raw(raw) };
                let element: IUIElement = object.cast()?;
                let mut opacity = 0.0;
                unsafe {
                    (element.vtable().GetOpacity)(element.as_raw(), &mut opacity).ok()?;
                    (element.vtable().SetOpacity)(element.as_raw(), 0.0).ok()?;
                }
                let mut state = tree.borrow_mut();
                state.saved.insert(handle, (element, opacity));
                state
                    .targets
                    .insert(handle, (frame, name == "BackgroundFill"));
                let fill = state
                    .targets
                    .values()
                    .any(|(owner, fill)| *owner == frame && *fill);
                let border = state
                    .targets
                    .values()
                    .any(|(owner, fill)| *owner == frame && !*fill);
                if fill && border {
                    unsafe {
                        SetEvent(this.ready.raw())?;
                    }
                }
                Ok(())
            })
        };
        if self.dispatcher.HasThreadAccess()? {
            action()
        } else {
            queue(self, action)
        }
    }

    fn restore(&self) -> Result<()> {
        self.active.store(false, Ordering::Release);
        let done = self.done.clone();
        queue(self, move || {
            let saved = TREE.with(|tree| std::mem::take(&mut tree.borrow_mut().saved));
            let mut failed = HashMap::new();
            let mut failure = None;
            for (handle, (element, opacity)) in saved {
                if let Err(error) =
                    unsafe { (element.vtable().SetOpacity)(element.as_raw(), opacity).ok() }
                {
                    failure = Some(error);
                    failed.insert(handle, (element, opacity));
                }
            }
            TREE.with(|tree| {
                if failed.is_empty() {
                    *tree.borrow_mut() = TreeState::default();
                } else {
                    tree.borrow_mut().saved = failed;
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
            SESSION_ACTIVE.store(false, Ordering::Release);
            unsafe {
                SetEvent(done.raw())?;
            }
            Ok(())
        })
    }
}

#[implement(ITreeCallback)]
struct Watcher {
    session: Session,
}
impl ITreeCallback_Impl for Watcher_Impl {
    unsafe fn OnVisualTreeChange(
        &self,
        relation: Relation,
        element: VisualElement,
        mutation: i32,
    ) -> HRESULT {
        if !self.session.active.load(Ordering::Acquire) {
            return S_OK;
        }
        let result = self.session.mutation(
            relation,
            element.handle,
            bstr_text(element.kind),
            bstr_text(element.name),
            mutation,
        );
        if result.is_err() {
            let _ = SetEvent(self.session.failed.raw());
        }
        result.into()
    }
}

#[implement(IObjectWithSite)]
struct Site {
    site: Mutex<Option<IUnknown>>,
}
impl IObjectWithSite_Impl for Site_Impl {
    fn SetSite(&self, site: Option<&IUnknown>) -> Result<()> {
        let Some(site) = site else {
            *self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))? = None;
            return Ok(());
        };
        if SESSION_ACTIVE.swap(true, Ordering::AcqRel) {
            return Err(Error::from(E_ILLEGAL_METHOD_CALL));
        }
        let result = (|| -> Result<()> {
            let diagnostics: IXamlDiagnostics = site.cast()?;
            let tree: ITreeService = site.cast()?;
            let mut raw = std::ptr::null_mut();
            unsafe {
                diagnostics.GetDispatcher(&mut raw).ok()?;
            }
            if raw.is_null() {
                return Err(Error::from(E_POINTER));
            }
            let dispatcher: CoreDispatcher = unsafe { IUnknown::from_raw(raw) }.cast()?;
            let mut init = std::ptr::null_mut();
            unsafe {
                diagnostics.GetInitializationData(&mut init).ok()?;
            }
            let initialization = unsafe { BSTR::from_raw(init) };
            let (host, shell, generation) =
                protocol::parse_init(&initialization.to_string()).map_err(Error::from)?;
            if shell != unsafe { GetCurrentProcessId() } {
                return Err(Error::from(E_INVALIDARG));
            }
            let open_event = |kind: &str, access| -> Result<Arc<OwnedHandle>> {
                let name = protocol::event_name(host, shell, generation, kind);
                unsafe {
                    OpenEventW(access, false, PCWSTR(name.as_ptr()))
                        .map(|handle| Arc::new(OwnedHandle(handle.0 as usize)))
                }
            };
            let ready = open_event("Ready", EVENT_MODIFY_STATE)?;
            let failed = open_event("Failed", EVENT_MODIFY_STATE)?;
            let done = open_event("Done", EVENT_MODIFY_STATE)?;
            let stop = open_event("Stop", SYNCHRONIZATION_SYNCHRONIZE)?;
            let process = Arc::new(OwnedHandle(
                unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, host)? }.0 as usize,
            ));
            let session = Session {
                diagnostics: AgileReference::new(&diagnostics)?,
                dispatcher,
                active: Arc::new(AtomicBool::new(true)),
                ready,
                failed,
                done,
            };
            let callback: ITreeCallback = Watcher {
                session: session.clone(),
            }
            .into();
            let tree = AgileReference::new(&tree)?;
            let callback = AgileReference::new(&callback)?;
            std::thread::Builder::new()
                .name("taskbar-watch".into())
                .spawn(move || {
                    unsafe {
                        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                            let _ = SetEvent(session.failed.raw());
                            SESSION_ACTIVE.store(false, Ordering::Release);
                            return;
                        }
                        let result = (|| -> Result<()> {
                            let tree = tree.resolve()?;
                            let callback = callback.resolve()?;
                            tree.AdviseVisualTreeChange(callback.as_raw()).ok()?;
                            let status = WaitForMultipleObjects(
                                &[process.raw(), stop.raw()],
                                false,
                                INFINITE,
                            );
                            if status == WAIT_FAILED {
                                return Err(Error::from_win32());
                            }
                            session.active.store(false, Ordering::Release);
                            tree.UnadviseVisualTreeChange(callback.as_raw()).ok()?;
                            session.restore()
                        })();
                        if result.is_err() {
                            let _ = SetEvent(session.failed.raw());
                            let _ = session.restore();
                        }
                        // Callback and queued UI closures own event handles until their last reference ends.
                        CoUninitialize();
                    }
                })
                .map_err(|error| Error::new(E_FAIL, error.to_string()))?;
            *self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))? = Some(site.clone());
            Ok(())
        })();
        if result.is_err() {
            SESSION_ACTIVE.store(false, Ordering::Release);
        }
        result
    }
    fn GetSite(&self, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if iid.is_null() || out.is_null() {
            return Err(Error::from(E_POINTER));
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        let site = self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
        let site = site.as_ref().ok_or_else(|| Error::from(E_FAIL))?;
        unsafe { site.query(iid, out).ok() }
    }
}

#[implement(IClassFactory)]
struct Factory;
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Option<&IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> Result<()> {
        if iid.is_null() || out.is_null() {
            return Err(Error::from(E_POINTER));
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if outer.is_some() {
            return Err(Error::from(CLASS_E_NOAGGREGATION));
        }
        let site: IObjectWithSite = Site {
            site: Mutex::new(None),
        }
        .into();
        unsafe { site.query(iid, out).ok() }
    }
    fn LockServer(&self, _: BOOL) -> Result<()> {
        Ok(())
    }
}

/// # Safety
/// COM supplies readable GUID pointers and a writable interface output pointer.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    class: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if class.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    *out = std::ptr::null_mut();
    if *class != protocol::BRIDGE_CLSID {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: IClassFactory = Factory.into();
    factory.query(iid, out)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xaml_abi_matches_windows_sdk_x64() {
        assert_eq!(std::mem::size_of::<Relation>(), 24);
        assert_eq!(std::mem::size_of::<SourceInfo>(), 32);
        assert_eq!(std::mem::size_of::<VisualElement>(), 64);
        assert_eq!(
            std::mem::offset_of!(IXamlDiagnostics_Vtbl, GetIInspectableFromHandle),
            6 * 8
        );
        assert_eq!(std::mem::offset_of!(IUIElement_Vtbl, GetOpacity), 9 * 8);
        assert_eq!(std::mem::offset_of!(IUIElement_Vtbl, SetOpacity), 10 * 8);
    }
}
