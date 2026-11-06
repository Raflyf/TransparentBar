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
#[interface("BAD9EB88-AE77-4397-B948-5FA2DB0A19EA")]
unsafe trait ITreeCallback2: ITreeCallback {
    fn OnElementStateChanged(&self, element: u64, state: i32, context: PCWSTR) -> HRESULT;
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use windows::{
    core::{implement, AgileReference, Error, Interface, Result, BSTR, GUID, PCWSTR},
    System::{DispatcherQueue, DispatcherQueueHandler},
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
    pending: HashMap<u64, bool>,
}
thread_local! { static TREE: RefCell<TreeState> = RefCell::new(TreeState::default()); }
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
enum UiDispatcher {
    Core(CoreDispatcher),
    Queue(DispatcherQueue),
}
impl UiDispatcher {
    fn from_object(object: &IUnknown) -> Result<Self> {
        if let Ok(queue) = object.cast::<DispatcherQueue>() {
            return Ok(Self::Queue(queue));
        }
        object.cast::<CoreDispatcher>().map(Self::Core)
    }
    fn has_thread_access(&self) -> Result<bool> {
        match self {
            Self::Core(dispatcher) => dispatcher.HasThreadAccess(),
            Self::Queue(dispatcher) => dispatcher.HasThreadAccess(),
        }
    }
    fn enqueue(&self, action: impl Fn() -> Result<()> + Send + 'static) -> Result<()> {
        match self {
            Self::Core(dispatcher) => {
                dispatcher.RunAsync(
                    CoreDispatcherPriority::Normal,
                    &DispatchedHandler::new(action),
                )?;
            }
            Self::Queue(dispatcher) => {
                if !dispatcher.TryEnqueue(&DispatcherQueueHandler::new(action))? {
                    return Err(Error::new(E_FAIL, "Dispatcher taskbar sedang berhenti."));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
struct OwnerState {
    dispatcher: UiDispatcher,
    ready: bool,
}

struct RestoreCompletion {
    remaining: AtomicUsize,
    failed: AtomicBool,
}
impl RestoreCompletion {
    fn new(count: usize) -> Self {
        Self {
            remaining: AtomicUsize::new(count),
            failed: AtomicBool::new(false),
        }
    }
    fn finish(&self, success: bool) -> bool {
        if !success {
            self.failed.store(true, Ordering::Release);
        }
        self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 && !self.failed.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
struct Session {
    diagnostics: AgileReference<IXamlDiagnostics>,
    fallback_dispatcher: UiDispatcher,
    owners: Arc<Mutex<HashMap<u32, OwnerState>>>,
    restoring: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    ready: Arc<OwnedHandle>,
    failed: Arc<OwnedHandle>,
    done: Arc<OwnedHandle>,
    inspect: bool,
    seen: Arc<AtomicBool>,
    status: Arc<protocol::StatusChannel>,
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

impl TreeState {
    fn candidates(&self) -> Vec<(u64, u64, bool)> {
        self.pending
            .iter()
            .filter_map(|(handle, fill)| {
                if self.targets.contains_key(handle) {
                    return None;
                }
                let parent = *self.nodes.get(handle)?;
                ancestor_frame(self, parent).map(|frame| (*handle, frame, *fill))
            })
            .collect()
    }
    fn has_background(&self) -> bool {
        self.targets
            .values()
            .any(|(frame, fill)| *fill && self.frames.contains(frame))
    }
}

impl Session {
    fn owner_dispatcher(&self) -> Result<Option<(u32, UiDispatcher)>> {
        let thread = unsafe { GetCurrentThreadId() };
        {
            let owners = self.owners.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
            if !self.active.load(Ordering::Acquire) {
                return Ok(None);
            }
            if let Some(owner) = owners.get(&thread) {
                return Ok(Some((thread, owner.dispatcher.clone())));
            }
        }
        let dispatcher = match DispatcherQueue::GetForCurrentThread() {
            Ok(queue) if queue.HasThreadAccess()? => UiDispatcher::Queue(queue),
            _ if self.fallback_dispatcher.has_thread_access()? => self.fallback_dispatcher.clone(),
            _ => {
                return Err(Error::new(
                    RPC_E_WRONG_THREAD,
                    "Callback XAML bukan pada UI thread pemilik.",
                ))
            }
        };
        let mut owners = self.owners.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
        if !self.active.load(Ordering::Acquire) {
            return Ok(None);
        }
        owners.insert(
            thread,
            OwnerState {
                dispatcher: dispatcher.clone(),
                ready: false,
            },
        );
        protocol::trace(&format!("Bridge.Owner thread={thread}"));
        Ok(Some((thread, dispatcher)))
    }

    fn update_ready(&self, thread: u32, ready: bool) -> Result<()> {
        let mut owners = self.owners.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(owner) = owners.get_mut(&thread) {
            owner.ready = ready;
        }
        let any_ready = owners.values().any(|owner| owner.ready);
        unsafe {
            if any_ready {
                self.status.stage(10);
                SetEvent(self.ready.raw())?;
            } else {
                ResetEvent(self.ready.raw())?;
            }
        }
        Ok(())
    }

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
        let Some((thread, dispatcher)) = self.owner_dispatcher()? else {
            return Ok(());
        };
        if !dispatcher.has_thread_access()? {
            return Err(Error::from(RPC_E_WRONG_THREAD));
        }
        let this = self;
        let action = || {
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
                    {
                        let mut state = tree.borrow_mut();
                        state.nodes.remove(&handle);
                        state.frames.remove(&handle);
                        state.targets.remove(&handle);
                        state.pending.remove(&handle);
                    }
                } else {
                    let mut state = tree.borrow_mut();
                    state.nodes.insert(handle, relation.parent);
                    if kind == "Taskbar.TaskbarFrame" {
                        if state.frames.insert(handle) {
                            this.status.data().frames.fetch_add(1, Ordering::Relaxed);
                        }
                        this.status.stage(8);
                    }
                    if kind == "Windows.UI.Xaml.Shapes.Rectangle"
                        && matches!(name.as_str(), "BackgroundFill" | "BackgroundStroke")
                    {
                        state.pending.insert(handle, name == "BackgroundFill");
                        if name == "BackgroundFill" {
                            this.status.stage(9);
                        }
                    }
                }
                let candidates = tree.borrow().candidates();
                for (target, frame, fill) in candidates {
                    let mut raw = std::ptr::null_mut();
                    unsafe {
                        diagnostics
                            .GetIInspectableFromHandle(target, &mut raw)
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
                        if !this.inspect {
                            // Keep the original before mutation; readback can fail after Windows accepts the setter.
                            tree.borrow_mut()
                                .saved
                                .entry(target)
                                .or_insert_with(|| (element.clone(), opacity));
                            (element.vtable().SetOpacity)(element.as_raw(), 0.0).ok()?;
                            let mut actual = f64::NAN;
                            (element.vtable().GetOpacity)(element.as_raw(), &mut actual).ok()?;
                            if actual != 0.0 {
                                return Err(Error::new(
                                    E_FAIL,
                                    "Readback opacity taskbar bukan 0.",
                                ));
                            }
                        }
                    }
                    let mut state = tree.borrow_mut();
                    state.targets.insert(target, (frame, fill));
                    if fill {
                        this.status.data().fills.fetch_add(1, Ordering::Relaxed);
                    } else {
                        this.status.data().borders.fetch_add(1, Ordering::Relaxed);
                    }
                }
                this.update_ready(thread, tree.borrow().has_background())?;
                Ok(())
            })
        };
        action()
    }

    fn restore(&self) -> Result<()> {
        if self.restoring.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.active.store(false, Ordering::Release);
        self.status.stage(11);
        let owners: Vec<_> = self
            .owners
            .lock()
            .map_err(|_| Error::from(E_UNEXPECTED))?
            .iter()
            .map(|(thread, owner)| (*thread, owner.dispatcher.clone()))
            .collect();
        if owners.is_empty() {
            SESSION_ACTIVE.store(false, Ordering::Release);
            self.status.stage(12);
            return unsafe { SetEvent(self.done.raw()) };
        }
        let completion = Arc::new(RestoreCompletion::new(owners.len()));
        for (thread, dispatcher) in owners {
            let session = self.clone();
            let remaining = completion.clone();
            let owner = dispatcher.clone();
            let action = move || {
                let result = (|| -> Result<()> {
                    if unsafe { GetCurrentThreadId() } != thread || !owner.has_thread_access()? {
                        return Err(Error::from(RPC_E_WRONG_THREAD));
                    }
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
                    Ok(())
                })();
                if let Err(error) = &result {
                    session.status.fail(error.code());
                    unsafe {
                        let _ = SetEvent(session.failed.raw());
                    }
                }
                if remaining.finish(result.is_ok()) {
                    SESSION_ACTIVE.store(false, Ordering::Release);
                    session.status.stage(12);
                    unsafe {
                        SetEvent(session.done.raw())?;
                    }
                }
                result
            };
            if let Err(error) = dispatcher.enqueue(action) {
                completion.finish(false);
                self.status.fail(error.code());
                unsafe {
                    let _ = SetEvent(self.failed.raw());
                }
            }
        }
        Ok(())
    }
}

#[implement(ITreeCallback2)]
struct Watcher {
    session: Session,
}
impl ITreeCallback2_Impl for Watcher_Impl {
    unsafe fn OnElementStateChanged(&self, _: u64, _: i32, _: PCWSTR) -> HRESULT {
        S_OK
    }
}

impl ITreeCallback_Impl for Watcher_Impl {
    unsafe fn OnVisualTreeChange(
        &self,
        relation: Relation,
        element: VisualElement,
        mutation: i32,
    ) -> HRESULT {
        // XAML diagnostics transfers these four BSTRs to the callback, including inactive callbacks.
        let _file = BSTR::from_raw(element.source.file);
        let _hash = BSTR::from_raw(element.source.hash);
        let kind_text = BSTR::from_raw(element.kind);
        let name_text = BSTR::from_raw(element.name);
        if !self.session.active.load(Ordering::Acquire) {
            return S_OK;
        }
        self.session
            .status
            .data()
            .callbacks
            .fetch_add(1, Ordering::Relaxed);
        self.session.status.stage(7);
        if !self.session.seen.swap(true, Ordering::AcqRel) {
            protocol::trace("Bridge.Callback received");
        }
        let kind = kind_text.to_string();
        let name = name_text.to_string();
        if kind == "Taskbar.TaskbarFrame"
            || matches!(name.as_str(), "BackgroundFill" | "BackgroundStroke")
        {
            protocol::trace(&format!(
                "Bridge.Target handle={} parent={} thread={} type={kind} name={name} mutation={mutation}",
                element.handle, relation.parent, GetCurrentThreadId()
            ));
        }
        let result = self
            .session
            .mutation(relation, element.handle, kind, name, mutation);
        if let Err(error) = &result {
            self.session.status.fail(error.code());
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
        protocol::trace("Bridge.SetSite");
        let Some(site) = site else {
            *self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))? = None;
            return Ok(());
        };
        let status = Arc::new(protocol::StatusChannel::open(unsafe {
            GetCurrentProcessId()
        })?);
        status.stage(2);
        if SESSION_ACTIVE.swap(true, Ordering::AcqRel) {
            status.fail(E_ILLEGAL_METHOD_CALL);
            return Err(Error::from(E_ILLEGAL_METHOD_CALL));
        }
        let result = (|| -> Result<()> {
            status.stage(3);
            protocol::trace("Bridge.QueryDiagnostics");
            let diagnostics: IXamlDiagnostics = site.cast()?;
            protocol::trace("Bridge.QueryTree");
            let tree: ITreeService = site.cast()?;
            let mut raw = std::ptr::null_mut();
            unsafe {
                protocol::trace("Bridge.GetDispatcher");
                diagnostics.GetDispatcher(&mut raw).ok()?;
            }
            if raw.is_null() {
                return Err(Error::from(E_POINTER));
            }
            status.stage(4);
            protocol::trace("Bridge.ResolveDispatcher");
            let dispatcher = UiDispatcher::from_object(&unsafe { IUnknown::from_raw(raw) })?;
            status.stage(5);
            let (host, shell, generation, inspect) = status.initialization()?;
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
                fallback_dispatcher: dispatcher,
                owners: Arc::new(Mutex::new(HashMap::new())),
                restoring: Arc::new(AtomicBool::new(false)),
                active: Arc::new(AtomicBool::new(true)),
                inspect,
                status: status.clone(),
                seen: Arc::new(AtomicBool::new(false)),
                ready,
                failed,
                done,
            };
            let callback: ITreeCallback2 = Watcher {
                session: session.clone(),
            }
            .into();
            let callback: ITreeCallback = callback.cast()?;
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
                            session.status.stage(6);
                            protocol::trace("Bridge.Advise");
                            tree.AdviseVisualTreeChange(callback.as_raw()).ok()?;
                            protocol::trace("Bridge.Advised");
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
                        if let Err(error) = &result {
                            session.status.fail(error.code());
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
        if let Err(error) = &result {
            status.fail(error.code());
            protocol::trace(&format!("Bridge.SetSite error={error}"));
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
    protocol::trace("Bridge.DllGetClassObject");
    let status = match protocol::StatusChannel::open(GetCurrentProcessId()) {
        Ok(status) => Arc::new(status),
        Err(error) => return error.code(),
    };
    status.stage(1);
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
    fn restore_waits_for_every_owner_and_never_hides_failure() {
        let completion = RestoreCompletion::new(2);
        assert!(!completion.finish(true));
        assert!(completion.finish(true));
        let completion = RestoreCompletion::new(2);
        assert!(!completion.finish(false));
        assert!(!completion.finish(true));
    }

    #[test]
    fn callback_threads_keep_separate_visual_trees() {
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [10u64, 20]
            .into_iter()
            .map(|frame| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    TREE.with(|tree| {
                        let mut tree = tree.borrow_mut();
                        tree.frames.insert(frame);
                        tree.nodes.insert(frame + 1, frame);
                        tree.pending.insert(frame + 1, true);
                    });
                    barrier.wait();
                    TREE.with(|tree| {
                        assert_eq!(tree.borrow().candidates(), vec![(frame + 1, frame, true)])
                    });
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn watcher_supports_multi_window_callback_contract() {
        fn requires_v2<T: ITreeCallback2_Impl>() {}
        requires_v2::<Watcher_Impl>();
        assert_eq!(
            ITreeCallback2::IID,
            GUID::from_u128(0xbad9eb88_ae77_4397_b948_5fa2db0a19ea)
        );
        assert_eq!(
            std::mem::offset_of!(ITreeCallback2_Vtbl, OnElementStateChanged),
            4 * 8
        );
    }

    #[test]
    fn late_frame_discovers_background_without_requiring_border() {
        let mut tree = TreeState::default();
        tree.nodes.insert(3, 2);
        tree.pending.insert(3, true);
        assert!(tree.candidates().is_empty());
        tree.nodes.insert(2, 1);
        tree.frames.insert(1);
        assert_eq!(tree.candidates(), vec![(3, 1, true)]);
        tree.targets.insert(3, (1, true));
        assert!(tree.has_background());
        assert!(tree.candidates().is_empty());
        tree.frames.remove(&1);
        assert!(!tree.has_background());
    }

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
