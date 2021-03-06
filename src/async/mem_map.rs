use std::ops::{Deref, DerefMut};
use core::{self, OclPrm, ClWaitListPtr, ClNullEventPtr, MemMap as MemMapCore, Mem as MemCore, AsMem};
use standard::{ClWaitListPtrEnum, ClNullEventPtrEnum, Event, EventList, Queue};
use async::{Result as AsyncResult};


/// An unmap command builder.
///
/// [UNSTABLE]
#[must_use = "commands do nothing unless enqueued"]
#[derive(Debug)]
pub struct MemUnmapCmd<'c, T> where T: 'c + OclPrm {
    queue: Option<&'c Queue>,
    mem_map: &'c mut MemMap<T>,
    ewait: Option<ClWaitListPtrEnum<'c>>,
    enew: Option<ClNullEventPtrEnum<'c>>,
}

impl<'c, T> MemUnmapCmd<'c, T> where T: OclPrm {
    /// Returns a new unmap command builder.
    fn new(mem_map: &'c mut MemMap<T>) -> MemUnmapCmd<'c, T>
    {
        MemUnmapCmd {
            queue: None,
            mem_map: mem_map,
            ewait: None,
            enew: None,
        }
    }

    /// Specifies a queue to use for this call only.
    pub fn queue<'q, Q>(mut self, queue: &'q Q) -> MemUnmapCmd<'c, T>
        where 'q: 'c, Q: 'q + AsRef<Queue>
    {
        self.queue = Some(queue.as_ref());
        self
    }


    /// Specifies a list of events to wait on before the command will run.
    pub fn ewait<EWL>(mut self, ewait: EWL) -> MemUnmapCmd<'c, T>
            where EWL: Into<ClWaitListPtrEnum<'c>>
    {
        self.ewait = Some(ewait.into());
        self
    }

    /// Specifies a list of events to wait on before the command will run or
    /// resets it to `None`.
    pub fn ewait_opt<EWL>(mut self, ewait: Option<EWL>) -> MemUnmapCmd<'c, T> where EWL: Into<ClWaitListPtrEnum<'c>> {
        self.ewait = ewait.map(|el| el.into());
        self
    }

    /// Specifies the destination for a new, optionally created event
    /// associated with this command.
    pub fn enew<NE>(mut self, enew: NE) -> MemUnmapCmd<'c, T>
            where NE: Into<ClNullEventPtrEnum<'c>>
    {
        self.enew = Some(enew.into());
        self
    }

    /// Specifies a destination for a new, optionally created event
    /// associated with this command or resets it to `None`.
    pub fn enew_opt<NE>(mut self, enew: Option<NE>) -> MemUnmapCmd<'c, T>
            where NE: Into<ClNullEventPtrEnum<'c>>
    {
        self.enew = enew.map(|e| e.into());
        self
    }

    /// Enqueues this command.
    ///
    pub fn enq(mut self) -> AsyncResult<()> {
        self.mem_map.enqueue_unmap(self.queue, self.ewait, self.enew)

    }
}


/// A view of memory mapped by `clEnqueueMap{...}`.
///
///
/// [UNSTABLE]: Still in a state of flux: ~90% stable
///
//
// [NOTE]: Do not derive/impl `Clone`. Will not be thread safe without a mutex.
//
#[derive(Debug)]
pub struct MemMap<T> where T: OclPrm {
    core: MemMapCore<T>,
    len: usize,
    buffer: MemCore,
    queue: Queue,
    unmap_wait_list: Option<EventList>,
    unmap_target_event: Option<Event>,
    callback_is_set: bool,
    is_unmapped: bool,
}

impl<T> MemMap<T>  where T: OclPrm {
    pub unsafe fn new(core: MemMapCore<T>, len: usize, unmap_wait_list: Option<EventList>,
        unmap_target_event: Option<Event>, buffer: MemCore, queue: Queue) -> MemMap<T>
    {
        MemMap {
            core: core,
            len: len,
            buffer: buffer,
            queue: queue,
            unmap_wait_list: unmap_wait_list,
            unmap_target_event: unmap_target_event,
            callback_is_set: false,
            is_unmapped: false,
        }
    }

    /// Returns an unmap command builder.
    ///
    /// Call `::enq` on it to enqueue the unmap command.
    pub fn unmap<'c>(&'c mut self) -> MemUnmapCmd<'c, T> {
        MemUnmapCmd::new(self)
    }

    /// Enqueues an unmap command for this memory object immediately.
    ///
    /// Prefer `::unmap` for a more stable interface as this function may
    /// change at any time.
    pub fn enqueue_unmap<Ewl, En>(&mut self, queue: Option<&Queue>, ewait_opt: Option<Ewl>,
            mut enew_opt: Option<En>) -> AsyncResult<()>
            where En: ClNullEventPtr, Ewl: ClWaitListPtr
    {
        if !self.is_unmapped {
            assert!(!(ewait_opt.is_some() && self.unmap_wait_list.is_some()), 
                "MemMap::enqueue_unmap: Cannot set an event wait list for the unmap command \
                when the 'unmap_wait_list' has already been set.");

            let mut origin_event_opt = if self.unmap_target_event.is_some() || enew_opt.is_some() {
                Some(Event::empty())
            } else {
                None
            };

            core::enqueue_unmap_mem_object(queue.unwrap_or(&self.queue), &self.buffer,
            &self.core, ewait_opt.and(self.unmap_wait_list.as_ref()), origin_event_opt.as_mut())?;
            
            self.is_unmapped = true;

            if let Some(origin_event) = origin_event_opt {
                // origin_event refcount: 1
                // If enew_opt is `Some`, update its internal event ptr.
                if let Some(ref mut enew) = enew_opt {
                        // origin_event/enew refcount: 2
                        unsafe { enew.clone_from(&origin_event) }
                }

                if cfg!(not(feature = "async_block")) {
                    // Async version:
                    if self.unmap_target_event.is_some() {
                        #[cfg(not(feature = "async_block"))]
                        self.register_event_trigger(&origin_event)?;

                        // `origin_event` will be reconstructed by the callback
                        // function using `UserEvent::from_raw` and `::drop`
                        // will be run there. Do not also run it here.
                        #[cfg(not(feature = "async_block"))]
                        ::std::mem::forget(origin_event);
                    }
                } else {
                    // Blocking version:
                    if let Some(ref mut um_tar) = self.unmap_target_event {
                        origin_event.wait_for()?;
                        um_tar.set_complete()?;
                    }
                }
            }

            Ok(())
        } else {
            Err("ocl_core::- ::unmap: Already unmapped.".into())
        }
    }

    #[cfg(not(feature = "async_block"))]
    fn register_event_trigger(&mut self, event: &Event) -> AsyncResult<()> {
        debug_assert!(self.is_unmapped && self.unmap_target_event.is_some());

        if !self.callback_is_set {
            if let Some(ref ev) = self.unmap_target_event {
                unsafe {
                    let unmap_target_event_ptr = ev.clone().into_raw();
                    event.set_callback(core::_complete_user_event, unmap_target_event_ptr)?;
                }

                self.callback_is_set = true;
                Ok(())
            } else {
                panic!("- ::register_event_trigger: No unmap event target \
                    has been configured with this MemMap.");
            }
        } else {
            Err("Callback already set.".into())
        }
    }

    /// Returns a reference to the unmap target event if it has been set.
    pub fn unmap_target_event(&self) -> Option<&Event> {
        self.unmap_target_event.as_ref()
    }

    /// Returns a reference to the unmap wait list if it has been set.
    pub fn unmap_wait_list(&self) -> Option<&EventList> {
        self.unmap_wait_list.as_ref()
    }

    /// Returns true if an unmap command has already been enqueued, causing
    /// the memory referenced by this `MemMap` to become invalid.
    #[inline] pub fn is_unmapped(&self) -> bool { self.is_unmapped }

    /// Returns a pointer to the host mapped memory.
    #[inline] pub fn as_ptr(&self) -> *const T { self.core.as_ptr() }

    /// Returns a mutable pointer to the host mapped memory.
    #[inline] pub fn as_mut_ptr(&mut self) -> *mut T { self.core.as_mut_ptr() }

    /// Returns a reference to the internal core command queue.
    #[inline] pub fn queue(&self) -> &Queue { &self.queue }
}

impl<T> Deref for MemMap<T> where T: OclPrm {
    type Target = [T];

    fn deref(&self) -> &[T] {
        assert!(!self.is_unmapped, "Mapped memory has been unmapped and cannot be accessed.");
        unsafe { self.core.as_slice(self.len) }
    }
}

impl<T> DerefMut for MemMap<T> where T: OclPrm {
    fn deref_mut(&mut self) -> &mut [T] {
        assert!(!self.is_unmapped, "Mapped memory has been unmapped and cannot be accessed.");
        unsafe { self.core.as_slice_mut(self.len) }
    }
}

impl<T: OclPrm> Drop for MemMap<T> {
    fn drop(&mut self) {
        if !self.is_unmapped {
            self.enqueue_unmap::<&Event, &mut Event>(None, None, None).ok();
        }
    }
}

impl<T: OclPrm> AsMem<T> for MemMap<T> {
    fn as_mem(&self) -> &MemCore {
        self.core.as_mem()
    }
}

// impl<'a, T: OclPrm> AsMem<T> for &'a mut MemMap<T> {
//     fn as_mem(&self) -> &MemCore {
//         self.core.as_mem()
//     }
// }