//! Test carriers with the runtime ABI's shapes. Allocations live in a thread
//! arena and are freed at thread exit. This fixture does not emulate a GC or
//! authorize concurrent guest-memory access.

use std::any::Any;
use std::cell::{RefCell, UnsafeCell};
use std::marker::PhantomData;

thread_local! {
    static ARENA: RefCell<Vec<Box<dyn Any>>> = const { RefCell::new(Vec::new()) };
}

fn allocate<T: 'static>(value: T) -> *mut T {
    let mut value = Box::new(value);
    let ptr = &mut *value as *mut T;
    ARENA.with(|arena| arena.borrow_mut().push(value));
    ptr
}

#[derive(Clone, Copy, Debug)]
pub enum ErrorKind {
    Type,
    Runtime,
}

pub mod host {
    use super::*;
    thread_local! {
        static RAISED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }
    pub fn raise(kind: ErrorKind, message: &str) {
        RAISED.with(|raised| raised.borrow_mut().push(format!("{kind:?}: {message}")));
    }
    pub fn raised() -> Vec<String> {
        RAISED.with(|raised| std::mem::take(&mut *raised.borrow_mut()))
    }
}

pub trait NativeEnum: Copy + Default {
    fn native(self) -> i32;
    fn from_native(value: i32) -> Option<Self>;
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Enum<T: NativeEnum>(i64, PhantomData<fn() -> T>);
impl<T: NativeEnum> Enum<T> {
    pub fn get(self) -> T {
        T::from_native(self.0 as i32).unwrap_or_default()
    }
}
impl<T: NativeEnum> From<T> for Enum<T> {
    fn from(value: T) -> Self {
        Self(i64::from(value.native()), PhantomData)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Text(*mut String);
impl Text {
    pub const NULL: Self = Self(std::ptr::null_mut());
    pub fn new(value: &str) -> Self {
        Self(allocate(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        if self.0.is_null() {
            ""
        } else {
            unsafe { &*self.0 }
        }
    }
    pub fn value(self) -> Value {
        Value(self)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Value(Text);

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Buffer(*mut UnsafeCell<Vec<u8>>);
impl Buffer {
    pub const NULL: Self = Self(std::ptr::null_mut());
    pub fn new(bytes: &[u8]) -> Self {
        Self(allocate(UnsafeCell::new(bytes.to_vec())))
    }
    pub fn len(&self) -> usize {
        if self.0.is_null() {
            0
        } else {
            unsafe { (&*(*self.0).get()).len() }
        }
    }
    pub fn as_ptr(&self) -> *const u8 {
        if self.0.is_null() {
            std::ptr::null()
        } else {
            unsafe { (&*(*self.0).get()).as_ptr() }
        }
    }
    pub unsafe fn as_slice(&self) -> &[u8] {
        if self.len() == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(self.as_ptr(), self.len()) }
        }
    }
    pub fn as_mut_ptr(&self) -> Option<*mut u8> {
        if self.0.is_null() {
            None
        } else {
            Some(unsafe { (&mut *(*self.0).get()).as_mut_ptr() })
        }
    }
    pub fn writable(self) -> BufferMut {
        BufferMut(self)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct BufferMut(Buffer);
impl BufferMut {
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn as_mut_ptr(&self) -> *mut u8 {
        if self.0.0.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { (&mut *(*self.0.0).get()).as_mut_ptr() }
        }
    }
    pub fn buffer(&self) -> Buffer {
        self.0
    }
}

#[repr(transparent)]
pub struct Future<T = ()>(
    *mut RefCell<Option<Result<Box<T>, String>>>,
    PhantomData<fn() -> T>,
);
impl<T> Copy for Future<T> {}
impl<T> Clone for Future<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: 'static> Future<T> {
    pub const NULL: Self = Self(std::ptr::null_mut(), PhantomData);
    pub fn new() -> Self {
        Self(allocate(RefCell::new(None)), PhantomData)
    }
    pub fn resolve_boxed(self, value: Box<T>) -> bool {
        self.settle(Ok(value))
    }
    pub fn reject(self, error: Value) -> bool {
        self.settle(Err(error.0.as_str().to_owned()))
    }
    fn settle(self, value: Result<Box<T>, String>) -> bool {
        let mut state = unsafe { &*self.0 }.borrow_mut();
        if state.is_some() {
            return false;
        }
        *state = Some(value);
        true
    }
    pub fn take(self) -> Option<Result<Box<T>, String>> {
        unsafe { &*self.0 }.borrow_mut().take()
    }
}

#[derive(Clone)]
pub struct Rooted<T: Clone>(T);
impl<T: Clone> Rooted<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
    pub fn get(&self) -> T {
        self.0.clone()
    }
}
