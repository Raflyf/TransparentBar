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
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ElementRole {
    Other,
    TaskbarFrame,
    BackgroundFill,
    BackgroundStroke,
}

#[inline]
fn wide_eq_ascii(wide: &[u16], ascii: &[u8]) -> bool {
    wide.len() == ascii.len()
        && wide
            .iter()
            .zip(ascii.iter())
            .all(|(&w, &b)| w == u16::from(b))
}

#[inline]
fn classify_element(kind: &[u16], name: &[u16]) -> ElementRole {
    if wide_eq_ascii(kind, b"Taskbar.TaskbarFrame") {
        ElementRole::TaskbarFrame
    } else if wide_eq_ascii(kind, b"Windows.UI.Xaml.Shapes.Rectangle") {
        if wide_eq_ascii(name, b"BackgroundFill") {
            ElementRole::BackgroundFill
        } else if wide_eq_ascii(name, b"BackgroundStroke") {
            ElementRole::BackgroundStroke
        } else {
            ElementRole::Other
        }
    } else {
        ElementRole::Other
    }
}

#[derive(Default)]
struct TreeState {
    epoch: u64,
    owner_registered: bool,
    last_ready: bool,
    last_dirty: bool,
    nodes: HashMap<u64, u64>,
    frames: HashSet<u64>,
    saved: HashMap<u64, (IUIElement, f64)>,
    targets: HashMap<u64, (u64, bool)>,
    pending: HashMap<u64, bool>,
}
thread_local! { static TREE: RefCell<TreeState> = RefCell::new(TreeState::default()); }
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);
static SESSION_EPOCH: AtomicU64 = AtomicU64::new(0);

/// RAII COM apartment guard ensuring COM objects in inner scopes drop before `CoUninitialize`.
struct ComGuard;
impl ComGuard {
    fn init_mta() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

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
                    return Err(Error::new(E_FAIL, "Taskbar dispatcher has stopped."));
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
    dirty: bool,
}

struct RestoreCompletion {
    remaining: AtomicUsize,
    failed: AtomicBool,
}
impl RestoreCompletion {
    fn new(count: usize, subscription_removed: bool) -> Self {
        Self {
            remaining: AtomicUsize::new(count),
            failed: AtomicBool::new(!subscription_removed),
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
    epoch: u64,
    diagnostics: AgileReference<IXamlDiagnostics>,
    fallback_dispatcher: UiDispatcher,
    owners: Arc<Mutex<HashMap<u32, OwnerState>>>,
    restoring: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    last_any_ready: Arc<AtomicBool>,
    ready: Arc<OwnedHandle>,
    failed: Arc<OwnedHandle>,
    done: Arc<OwnedHandle>,
    inspect: bool,
    seen: Arc<AtomicBool>,
    status: Arc<protocol::StatusChannel>,
    log_tx: SyncSender<String>,
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

fn reaches_root_without_frame(state: &TreeState, mut handle: u64) -> bool {
    for _ in 0..64 {
        if state.frames.contains(&handle) {
            return false;
        }
        let Some(&parent) = state.nodes.get(&handle) else {
            return false;
        };
        if parent == 0 || parent == handle {
            return true;
        }
        handle = parent;
    }
    false
}

impl TreeState {
    fn prepare(&mut self, epoch: u64) -> Result<()> {
        if self.epoch == epoch {
            return Ok(());
        }
        if !self.saved.is_empty() {
            return Err(Error::new(
                E_ILLEGAL_METHOD_CALL,
                "Previous session taskbar backup has not been restored yet.",
            ));
        }
        // `saved` is guaranteed empty here, so replacing `*self` drops zero COM objects.
        *self = Self {
            epoch,
            ..Self::default()
        };
        Ok(())
    }
    fn candidates(&self) -> Vec<(u64, u64, bool)> {
        if self.pending.is_empty() || self.frames.is_empty() {
            return Vec::new();
        }
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
    fn prune_non_frame_pending(&mut self) {
        if self.pending.is_empty() || self.frames.is_empty() {
            return;
        }
        let drop_keys: Vec<u64> = self
            .pending
            .keys()
            .copied()
            .filter(|handle| {
                self.targets.contains_key(handle)
                    || self
                        .nodes
                        .get(handle)
                        .is_some_and(|&parent| reaches_root_without_frame(self, parent))
            })
            .collect();
        for key in drop_keys {
            self.pending.remove(&key);
        }
    }
    fn has_background(&self) -> bool {
        self.targets
            .values()
            .any(|(frame, fill)| *fill && self.frames.contains(frame))
    }
}

fn flush_logs(rx: &mpsc::Receiver<String>) {
    for entry in rx.try_iter() {
        protocol::trace(&entry);
    }
}

impl Session {
    fn log(&self, entry: impl Into<String>) {
        // Non-blocking send to worker thread; never perform synchronous disk I/O on Explorer's UI thread.
        let _ = self.log_tx.try_send(entry.into());
    }

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
                    "XAML callback is not on the owning UI thread.",
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
                dirty: false,
            },
        );
        self.log(format!("Bridge.Owner thread={thread}"));
        Ok(Some((thread, dispatcher)))
    }

    fn begin_change(&self, thread: u32) -> Result<bool> {
        let mut owners = self.owners.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
        if !self.active.load(Ordering::Acquire) {
            return Ok(false);
        }
        let owner = owners
            .get_mut(&thread)
            .ok_or_else(|| Error::from(E_UNEXPECTED))?;
        owner.dirty = true;
        Ok(true)
    }

    fn update_ready(&self, thread: u32, ready: bool, dirty: bool) -> Result<()> {
        let mut owners = self.owners.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(owner) = owners.get_mut(&thread) {
            owner.ready = ready;
            owner.dirty = dirty;
        }
        let any_ready = owners.values().any(|owner| owner.ready);
        let prev = self.last_any_ready.swap(any_ready, Ordering::AcqRel);
        if any_ready != prev {
            unsafe {
                if any_ready {
                    self.status.stage(10);
                    SetEvent(self.ready.raw())?;
                } else {
                    ResetEvent(self.ready.raw())?;
                }
            }
        }
        Ok(())
    }

    fn mutation(
        &self,
        relation: Relation,
        handle: u64,
        role: ElementRole,
        mutation: i32,
    ) -> Result<()> {
        if !self.active.load(Ordering::Acquire) {
            return Ok(());
        }
        let thread = unsafe { GetCurrentThreadId() };
        // Every TREE borrow is short-lived and dropped strictly before any COM call or IUIElement drop.
        let already_registered = TREE.with(|tree| -> Result<bool> {
            let mut state = tree
                .try_borrow_mut()
                .map_err(|_| Error::from(E_UNEXPECTED))?;
            state.prepare(self.epoch)?;
            Ok(state.owner_registered)
        })?;
        if !already_registered {
            let Some((_, dispatcher)) = self.owner_dispatcher()? else {
                return Ok(());
            };
            if !dispatcher.has_thread_access()? {
                return Err(Error::from(RPC_E_WRONG_THREAD));
            }
            if !self.active.load(Ordering::Acquire) {
                return Ok(());
            }
            TREE.with(|tree| -> Result<()> {
                let mut state = tree
                    .try_borrow_mut()
                    .map_err(|_| Error::from(E_UNEXPECTED))?;
                state.owner_registered = true;
                Ok(())
            })?;
        }

        if mutation != 0 {
            let removed: Option<(IUIElement, f64)> = TREE.with(|tree| -> Result<_> {
                let mut state = tree
                    .try_borrow_mut()
                    .map_err(|_| Error::from(E_UNEXPECTED))?;
                let saved = state.saved.remove(&handle);
                state.nodes.remove(&handle);
                state.frames.remove(&handle);
                state.targets.remove(&handle);
                state.pending.remove(&handle);
                Ok(saved)
            })?;
            if let Some((element, opacity)) = removed {
                let set_res =
                    unsafe { (element.vtable().SetOpacity)(element.as_raw(), opacity).ok() };
                if let Err(error) = set_res {
                    let displaced = TREE.with(|tree| {
                        tree.try_borrow_mut()
                            .ok()
                            .and_then(|mut s| s.saved.insert(handle, (element, opacity)))
                    });
                    drop(displaced);
                    return Err(error);
                }
                // `element` drops here with zero RefCell borrows held.
                drop(element);
            }
        } else {
            let candidates = TREE.with(|tree| -> Result<Vec<(u64, u64, bool)>> {
                let mut state = tree
                    .try_borrow_mut()
                    .map_err(|_| Error::from(E_UNEXPECTED))?;
                state.nodes.insert(handle, relation.parent);
                match role {
                    ElementRole::TaskbarFrame => {
                        if state.frames.insert(handle) {
                            self.status.data().frames.fetch_add(1, Ordering::Relaxed);
                            self.log(format!(
                                "Bridge.Target handle={handle} parent={} thread={thread} type=Taskbar.TaskbarFrame",
                                relation.parent
                            ));
                        }
                        self.status.stage(8);
                    }
                    ElementRole::BackgroundFill => {
                        state.pending.insert(handle, true);
                        self.status.stage(9);
                    }
                    ElementRole::BackgroundStroke => {
                        state.pending.insert(handle, false);
                    }
                    ElementRole::Other => {}
                }
                if state.pending.is_empty() || state.frames.is_empty() {
                    return Ok(Vec::new());
                }
                let found = state.candidates();
                state.prune_non_frame_pending();
                Ok(found)
            })?;

            if !candidates.is_empty() {
                let diagnostics = self.diagnostics.resolve()?;
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
                        if !self.inspect {
                            if !self.begin_change(thread)? {
                                return Ok(());
                            }
                            // Clone COM interface OUTSIDE any RefCell borrow.
                            let backup = element.clone();
                            let unused_backup: Option<IUIElement> =
                                TREE.with(|tree| -> Result<_> {
                                    let mut state = tree
                                        .try_borrow_mut()
                                        .map_err(|_| Error::from(E_UNEXPECTED))?;
                                    if let std::collections::hash_map::Entry::Vacant(slot) =
                                        state.saved.entry(target)
                                    {
                                        slot.insert((backup, opacity));
                                        Ok(None)
                                    } else {
                                        Ok(Some(backup))
                                    }
                                })?;
                            drop(unused_backup);
                            (element.vtable().SetOpacity)(element.as_raw(), 0.0).ok()?;
                            let mut actual = f64::NAN;
                            (element.vtable().GetOpacity)(element.as_raw(), &mut actual).ok()?;
                            if actual != 0.0 {
                                return Err(Error::new(
                                    E_FAIL,
                                    "Taskbar opacity readback is not 0.",
                                ));
                            }
                        }
                    }
                    TREE.with(|tree| -> Result<()> {
                        let mut state = tree
                            .try_borrow_mut()
                            .map_err(|_| Error::from(E_UNEXPECTED))?;
                        state.targets.insert(target, (frame, fill));
                        state.pending.remove(&target);
                        Ok(())
                    })?;
                    self.log(format!(
                        "Bridge.Target handle={target} frame={frame} thread={thread} role={}",
                        if fill {
                            "BackgroundFill"
                        } else {
                            "BackgroundStroke"
                        }
                    ));
                    if fill {
                        self.status.data().fills.fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.status.data().borders.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }

        let ready_change = TREE.with(|tree| -> Result<Option<(bool, bool)>> {
            let mut state = tree
                .try_borrow_mut()
                .map_err(|_| Error::from(E_UNEXPECTED))?;
            let has_background = state.has_background();
            let has_saved = !state.saved.is_empty();
            if has_background != state.last_ready || has_saved != state.last_dirty {
                state.last_ready = has_background;
                state.last_dirty = has_saved;
                Ok(Some((has_background, has_saved)))
            } else {
                Ok(None)
            }
        })?;
        if let Some((has_background, has_saved)) = ready_change {
            self.update_ready(thread, has_background, has_saved)?;
        }
        Ok(())
    }

    fn restore(&self, subscription_removed: bool) -> Result<()> {
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
            .filter(|(_, owner)| owner.dirty)
            .map(|(thread, owner)| (*thread, owner.dispatcher.clone()))
            .collect();
        if owners.is_empty() {
            if !subscription_removed {
                return Ok(());
            }
            SESSION_ACTIVE.store(false, Ordering::Release);
            self.status.stage(12);
            return unsafe { SetEvent(self.done.raw()) };
        }
        let completion = Arc::new(RestoreCompletion::new(owners.len(), subscription_removed));
        for (thread, dispatcher) in owners {
            let session = self.clone();
            let remaining = completion.clone();
            let owner = dispatcher.clone();
            let action = move || {
                let guarded = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
                    if unsafe { GetCurrentThreadId() } != thread || !owner.has_thread_access()? {
                        return Err(Error::from(RPC_E_WRONG_THREAD));
                    }
                    let saved = TREE.with(|tree| -> Result<_> {
                        let mut state = tree
                            .try_borrow_mut()
                            .map_err(|_| Error::from(E_UNEXPECTED))?;
                        Ok(std::mem::take(&mut state.saved))
                    })?;
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
                    // Extract old TreeState / saved map out of RefCell before dropping any COM objects.
                    let extracted_for_drop: Option<TreeState> = TREE.with(|tree| {
                        let Ok(mut state) = tree.try_borrow_mut() else {
                            return None;
                        };
                        if failed.is_empty() {
                            Some(std::mem::take(&mut *state))
                        } else {
                            let old_saved = std::mem::replace(&mut state.saved, failed);
                            Some(TreeState {
                                saved: old_saved,
                                ..TreeState::default()
                            })
                        }
                    });
                    drop(extracted_for_drop);
                    if let Some(error) = failure {
                        return Err(error);
                    }
                    Ok(())
                }));
                let result = match guarded {
                    Ok(res) => res,
                    Err(_) => Err(Error::from(E_FAIL)),
                };
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
                        let _ = SetEvent(session.done.raw());
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
        catch_unwind(AssertUnwindSafe(|| S_OK)).unwrap_or(E_FAIL)
    }
}

impl ITreeCallback_Impl for Watcher_Impl {
    unsafe fn OnVisualTreeChange(
        &self,
        relation: Relation,
        element: VisualElement,
        mutation: i32,
    ) -> HRESULT {
        catch_unwind(AssertUnwindSafe(|| {
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
                self.session.log("Bridge.Callback received");
            }
            // Zero-copy UTF-16 matching avoids two heap String allocations per XAML callback.
            let role = classify_element(kind_text.as_wide(), name_text.as_wide());
            if self.session.inspect && role != ElementRole::Other {
                self.session.log(format!(
                    "Bridge.Diagnose handle={} parent={} thread={} role={role:?} mutation={mutation}",
                    element.handle,
                    relation.parent,
                    GetCurrentThreadId()
                ));
            }
            let result = self
                .session
                .mutation(relation, element.handle, role, mutation);
            if let Err(error) = &result {
                self.session.status.fail(error.code());
                let _ = SetEvent(self.session.failed.raw());
            }
            result.into()
        }))
        .unwrap_or_else(|_| {
            self.session.status.fail(E_FAIL);
            let _ = SetEvent(self.session.failed.raw());
            E_FAIL
        })
    }
}

#[implement(IObjectWithSite)]
struct Site {
    site: Mutex<Option<IUnknown>>,
}
impl IObjectWithSite_Impl for Site_Impl {
    fn SetSite(&self, site: Option<&IUnknown>) -> Result<()> {
        match catch_unwind(AssertUnwindSafe(|| self.set_site_inner(site))) {
            Ok(res) => res,
            Err(_) => {
                SESSION_ACTIVE.store(false, Ordering::Release);
                Err(Error::from(E_FAIL))
            }
        }
    }

    fn GetSite(&self, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        match catch_unwind(AssertUnwindSafe(|| {
            if iid.is_null() || out.is_null() {
                return Err(Error::from(E_POINTER));
            }
            unsafe {
                *out = std::ptr::null_mut();
            }
            let site = self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))?;
            let site = site.as_ref().ok_or_else(|| Error::from(E_FAIL))?;
            unsafe { site.query(iid, out).ok() }
        })) {
            Ok(res) => res,
            Err(_) => Err(Error::from(E_FAIL)),
        }
    }
}

impl Site_Impl {
    fn set_site_inner(&self, site: Option<&IUnknown>) -> Result<()> {
        let Some(site) = site else {
            *self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))? = None;
            return Ok(());
        };
        let diagnostics: IXamlDiagnostics = site.cast()?;
        let mut init_raw = std::ptr::null_mut();
        unsafe {
            diagnostics.GetInitializationData(&mut init_raw).ok()?;
        }
        if init_raw.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let init_bstr = unsafe { BSTR::from_raw(init_raw) };
        let init = protocol::parse_init_data(&init_bstr.to_string())?;
        let current_pid = unsafe { GetCurrentProcessId() };
        if init.shell != current_pid {
            return Err(Error::from(E_INVALIDARG));
        }

        let status = Arc::new(protocol::StatusChannel::open(&init.token, current_pid)?);
        status.stage(2);
        if SESSION_ACTIVE.swap(true, Ordering::AcqRel) {
            status.fail(E_ILLEGAL_METHOD_CALL);
            return Err(Error::from(E_ILLEGAL_METHOD_CALL));
        }
        let result = (|| -> Result<()> {
            status.stage(3);
            let tree: ITreeService = site.cast()?;
            let mut raw = std::ptr::null_mut();
            unsafe {
                diagnostics.GetDispatcher(&mut raw).ok()?;
            }
            if raw.is_null() {
                return Err(Error::from(E_POINTER));
            }
            status.stage(4);
            let dispatcher = UiDispatcher::from_object(&unsafe { IUnknown::from_raw(raw) })?;
            status.stage(5);
            let (host, shell, generation, inspect) = status.initialization()?;
            if host != init.host
                || shell != current_pid
                || generation != init.generation
                || inspect != init.inspect
            {
                return Err(Error::from(E_INVALIDARG));
            }
            let open_event = |kind: &str, access| -> Result<Arc<OwnedHandle>> {
                let name = protocol::event_name(&init.token, kind);
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
            let (log_tx, log_rx) = mpsc::sync_channel::<String>(64);
            let session = Session {
                epoch: SESSION_EPOCH.fetch_add(1, Ordering::AcqRel) + 1,
                diagnostics: AgileReference::new(&diagnostics)?,
                fallback_dispatcher: dispatcher,
                owners: Arc::new(Mutex::new(HashMap::new())),
                restoring: Arc::new(AtomicBool::new(false)),
                active: Arc::new(AtomicBool::new(true)),
                last_any_ready: Arc::new(AtomicBool::new(false)),
                inspect,
                status: status.clone(),
                seen: Arc::new(AtomicBool::new(false)),
                ready,
                failed,
                done,
                log_tx,
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
                .stack_size(256 * 1024)
                .spawn(move || {
                    let failed_handle = session.failed.clone();
                    let status_ref = session.status.clone();
                    let outer = catch_unwind(AssertUnwindSafe(move || {
                        let _com = match ComGuard::init_mta() {
                            Ok(guard) => guard,
                            Err(error) => {
                                status_ref.fail(error.code());
                                unsafe {
                                    let _ = SetEvent(failed_handle.raw());
                                }
                                SESSION_ACTIVE.store(false, Ordering::Release);
                                return;
                            }
                        };
                        // Move ALL COM-holding objects (`tree`, `callback`, `session`) into an inner
                        // scope/closure so they are guaranteed to drop strictly BEFORE `_com` calls
                        // `CoUninitialize()`, even if an unexpected panic unwinds.
                        let worker_failed = session.failed.clone();
                        let worker_status = session.status.clone();
                        let inner = catch_unwind(AssertUnwindSafe(move || {
                            let mut subscription_removed = true;
                            let result = (|| -> Result<()> {
                                let tree = tree.resolve()?;
                                let callback = callback.resolve()?;
                                session.status.stage(6);
                                protocol::trace("Bridge.Advise");
                                subscription_removed = false;
                                // Advise can register before failing during initial enumeration.
                                let operation = (|| -> Result<()> {
                                    unsafe {
                                        tree.AdviseVisualTreeChange(callback.as_raw()).ok()?;
                                    }
                                    protocol::trace("Bridge.Advised");
                                    flush_logs(&log_rx);
                                    unsafe {
                                        if WaitForMultipleObjects(
                                            &[process.raw(), stop.raw()],
                                            false,
                                            INFINITE,
                                        ) == WAIT_FAILED
                                        {
                                            return Err(Error::from_win32());
                                        }
                                    }
                                    Ok(())
                                })();
                                session.active.store(false, Ordering::Release);
                                let removal = unsafe {
                                    tree.UnadviseVisualTreeChange(callback.as_raw()).ok()
                                };
                                subscription_removed = removal.is_ok();
                                if let Err(error) = &removal {
                                    session.status.fail(error.code());
                                }
                                operation.and(removal)
                            })();
                            if let Err(error) = &result {
                                session.status.fail(error.code());
                                unsafe {
                                    let _ = SetEvent(session.failed.raw());
                                }
                            }
                            if let Err(error) = session.restore(subscription_removed) {
                                session.status.fail(error.code());
                                unsafe {
                                    let _ = SetEvent(session.failed.raw());
                                }
                            }
                            flush_logs(&log_rx);
                            // `tree`, `callback`, `session`, `process`, `stop` drop at the end of this closure,
                            // BEFORE `_com` runs `CoUninitialize()`.
                        }));
                        if inner.is_err() {
                            worker_status.fail(E_FAIL);
                            unsafe {
                                let _ = SetEvent(worker_failed.raw());
                            }
                            SESSION_ACTIVE.store(false, Ordering::Release);
                        }
                    }));
                    if outer.is_err() {
                        SESSION_ACTIVE.store(false, Ordering::Release);
                    }
                })
                .map_err(|error| Error::new(E_FAIL, error.to_string()))?;
            *self.site.lock().map_err(|_| Error::from(E_UNEXPECTED))? = Some(site.clone());
            Ok(())
        })();
        if let Err(error) = &result {
            status.fail(error.code());
            SESSION_ACTIVE.store(false, Ordering::Release);
        }
        result
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
        match catch_unwind(AssertUnwindSafe(|| {
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
        })) {
            Ok(res) => res,
            Err(_) => Err(Error::from(E_FAIL)),
        }
    }
    fn LockServer(&self, _: BOOL) -> Result<()> {
        Ok(())
    }
}

/// # Safety
/// Called by the OS loader under the loader lock; must never unwind or invoke COM/blocking APIs.
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    _instance: HMODULE,
    _reason: u32,
    _reserved: *mut c_void,
) -> BOOL {
    catch_unwind(AssertUnwindSafe(|| TRUE)).unwrap_or(FALSE)
}

/// # Safety
/// COM supplies readable GUID pointers and a writable interface output pointer.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    class: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        if class.is_null() || iid.is_null() || out.is_null() {
            return E_POINTER;
        }
        *out = std::ptr::null_mut();
        if *class != protocol::BRIDGE_CLSID {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        factory.query(iid, out)
    }))
    .unwrap_or(E_FAIL)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    catch_unwind(|| S_FALSE).unwrap_or(S_FALSE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_clears_clean_thread_cache() {
        let mut tree = TreeState::default();
        tree.prepare(1).unwrap();
        tree.frames.insert(10);
        tree.nodes.insert(11, 10);
        tree.pending.insert(11, true);
        tree.prepare(2).unwrap();
        assert_eq!(tree.epoch, 2);
        assert!(tree.frames.is_empty() && tree.nodes.is_empty() && tree.pending.is_empty());
    }

    #[test]
    fn restore_waits_for_every_owner_and_never_hides_failure() {
        let completion = RestoreCompletion::new(2, true);
        assert!(!completion.finish(true));
        assert!(completion.finish(true));
        let completion = RestoreCompletion::new(2, true);
        assert!(!completion.finish(false));
        assert!(!completion.finish(true));
        let completion = RestoreCompletion::new(2, false);
        assert!(!completion.finish(true));
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
        assert_eq!(
            std::mem::offset_of!(IXamlDiagnostics_Vtbl, GetInitializationData),
            10 * 8
        );
        assert_eq!(std::mem::offset_of!(IUIElement_Vtbl, GetOpacity), 9 * 8);
        assert_eq!(std::mem::offset_of!(IUIElement_Vtbl, SetOpacity), 10 * 8);
    }

    #[test]
    fn com_guard_drops_after_inner_objects_even_on_unwind() {
        let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        struct Tracker(Arc<Mutex<Vec<&'static str>>>, &'static str);
        impl Drop for Tracker {
            fn drop(&mut self) {
                self.0.lock().unwrap().push(self.1);
            }
        }
        let guard = Tracker(events.clone(), "com_uninit");
        let com_obj = Tracker(events.clone(), "com_object");
        let res = catch_unwind(AssertUnwindSafe(move || {
            let _obj = com_obj;
            panic!("simulated failure");
        }));
        assert!(res.is_err());
        drop(guard);
        assert_eq!(*events.lock().unwrap(), vec!["com_object", "com_uninit"]);
    }

    #[test]
    fn classify_element_matches_utf16_without_allocation() {
        let to_u16 = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        assert_eq!(
            classify_element(&to_u16("Taskbar.TaskbarFrame"), &[]),
            ElementRole::TaskbarFrame
        );
        assert_eq!(
            classify_element(
                &to_u16("Windows.UI.Xaml.Shapes.Rectangle"),
                &to_u16("BackgroundFill")
            ),
            ElementRole::BackgroundFill
        );
        assert_eq!(
            classify_element(
                &to_u16("Windows.UI.Xaml.Shapes.Rectangle"),
                &to_u16("BackgroundStroke")
            ),
            ElementRole::BackgroundStroke
        );
        assert_eq!(
            classify_element(
                &to_u16("Windows.UI.Xaml.Controls.Grid"),
                &to_u16("BackgroundFill")
            ),
            ElementRole::Other
        );
    }

    #[test]
    fn prune_non_frame_pending_drops_root_controls_and_keeps_unresolved_parents() {
        let mut tree = TreeState::default();
        tree.frames.insert(10);
        tree.nodes.insert(10, 0);
        // Non-taskbar tooltip control connected all the way to root 0.
        tree.nodes.insert(200, 0);
        tree.nodes.insert(201, 200);
        tree.pending.insert(201, true);
        // Unresolved late child whose parent 50 has not arrived yet.
        tree.nodes.insert(51, 50);
        tree.pending.insert(51, true);
        // Already-applied target still in pending gets pruned too.
        tree.nodes.insert(11, 10);
        tree.targets.insert(11, (10, true));
        tree.pending.insert(11, true);

        tree.prune_non_frame_pending();
        assert!(!tree.pending.contains_key(&201));
        assert!(!tree.pending.contains_key(&11));
        assert!(tree.pending.contains_key(&51));
    }
}
