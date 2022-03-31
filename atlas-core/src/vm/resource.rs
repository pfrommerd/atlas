use crate::store::Storage;
use crate::{Error, ErrorKind};
use crate::store::value::Value;

use std::ops::Deref;
use std::rc::Rc;
use url::Url;
use std::collections::HashMap;
use std::cell::RefCell;
use bytes::Bytes;

use async_trait::async_trait;

#[async_trait(?Send)]
pub trait ResourceProvider<'s, S: Storage> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error>;
}

pub struct Resources<'s, S : Storage + 's> {
    providers: Vec<Rc<dyn ResourceProvider<'s, S> + 's>>
}

impl<'s, S: Storage + 's> Resources<'s, S> {
    pub fn new() -> Resources<'s, S> {
        Self { providers: Default::default() }
    }
    pub fn add_provider(&mut self, prov: Rc<dyn ResourceProvider<'s, S> + 's>) {
        self.providers.push(prov);
    }
}

#[async_trait(?Send)]
impl<'s, S : Storage> ResourceProvider<'s, S> for Resources<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        log::trace!(target: "resource", "fetching {}", res);
        let result = {
            for p in self.providers.iter() {
                match p.retrieve(res).await {
                    Ok(h) => return Ok(h),
                    _ => ()
                }
            }
            Err(Error::new_const(ErrorKind::NotFound, "Resource not found"))
        };
        match &result {
        Err(e) => log::trace!(target: "resource", "resource {} gave error: {:?}", res, e),
        Ok(_) => ()
        }
        result
    }
}

pub struct Snapshot<'s, S : Storage + 's> {
    snapshot : RefCell<HashMap<Url, S::Handle<'s>>>,
    resources: Rc<dyn ResourceProvider<'s, S> + 's>
}

impl<'s, S> Snapshot<'s, S> 
        where S: Storage + 's {
    
    pub fn new(resources: Rc<dyn ResourceProvider<'s, S> + 's>) -> Self {
        Self { snapshot: RefCell::new(HashMap::new()), resources }
    }
}

#[async_trait(?Send)]
impl<'s, S: Storage + 's> ResourceProvider<'s, S> for Snapshot<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        let mut snapshot = self.snapshot.borrow_mut();
        match snapshot.get(&res) {
            Some(h) => Ok(h.clone()),
            None => {
                match self.resources.retrieve(res).await {
                Ok(h) => {
                    // Insert into the snapshot table
                    snapshot.insert(res.clone(), h.clone());
                    Ok(h)
                },
                Err(e) => Err(e)
                }
            }
        }
    }
}

pub struct FileProvider<'s, S: Storage> {
    store: &'s S
}

impl<'s, S: Storage> FileProvider<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self { store }
    }
}

#[async_trait(?Send)]
impl<'s, S: Storage + 's> ResourceProvider<'s, S> for FileProvider<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        if res.scheme() != "file" {
            return Err(Error::new_const(ErrorKind::NotFound, "Only supports file:// scheme"))
        }
        let res = std::fs::read(res.path())
            .map_err(|_| Error::new_const(ErrorKind::IO, "Couldn't read file"))?;
        let val = Value::Buffer(Bytes::from(res));
        self.store.insert_from(&val)
    }
}

pub struct HttpProvider<'s, S: Storage> {
    store: &'s S,
    cache: RefCell<HashMap<Url, S::Handle<'s>>>
}

impl<'s, S: Storage> HttpProvider<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self { store, cache: RefCell::new(HashMap::new()) }
    }
}

#[async_trait(?Send)]
impl<'s, S: Storage + 's> ResourceProvider<'s, S> for HttpProvider<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        if res.scheme() != "http" && res.scheme() != "https" {
            return Err(Error::new_const(ErrorKind::NotFound, "Only supports file:// scheme"))
        }
        {
            let cache = self.cache.borrow_mut();
            if let Some(h) = cache.get(res) {
                return Ok(h.clone())
            }
        }
        let response = surf::get(res);
        let bytes = response.recv_bytes().await.map_err(|_| Error::new("Unable to fetch"))?;
        let val = Value::Buffer(Bytes::from(bytes));
        let handle = self.store.insert_from(&val)?;
        self.cache.borrow_mut().insert(res.clone(), handle.clone());
        Ok(handle)
    }
}

pub struct BuiltinsProvider<'s, S: Storage + 's> {
    store: &'s S,
    handle: RefCell<Option<S::Handle<'s>>>
}

impl<'s, S: Storage + 's> BuiltinsProvider<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self { store, handle: RefCell::new(None) }
    }

    async fn prelude(&self) -> Result<S::Handle<'s>, Error> {
        {
            let opt = self.handle.borrow();
            if let Some(h) = opt.deref() {
                return Ok(h.clone())
            }
        }
        let mut opt = self.handle.borrow_mut();
        // we need to compile the prelude
        let prelude = String::from(crate::core::prelude::PRELUDE);
        let handle = self.store.insert_from(&Value::String(prelude))?;
        *opt = Some(handle.clone());
        Ok(handle)
    }
}

#[async_trait(?Send)]
impl<'s, S: Storage + 's> ResourceProvider<'s, S> for BuiltinsProvider<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        if res.scheme() != "builtin" {
            return Err(Error::new_const(ErrorKind::NotFound, "Only supports builtin:// scheme"))
        }
        if res.host_str() == Some("prelude") {
            return self.prelude().await
        } else {
            return Err(Error::new_const(ErrorKind::NotFound, "Builtin not found"))
        }
    }
}