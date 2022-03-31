use crate::store::{Storage, Storable};
use crate::{Error, ErrorKind};
use crate::store::value::Value;
use crate::compile::{Env, Compile};

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
    providers: Vec<Rc<dyn ResourceProvider<'s, S>>>
}

#[async_trait(?Send)]
impl<'s, S : Storage> ResourceProvider<'s, S> for Resources<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        for p in self.providers.iter() {
            match p.retrieve(res).await {
                Ok(h) => return Ok(h),
                _ => ()
            }
        }
        Err(Error::new_const(ErrorKind::NotFound, "Resource not found"))
    }
}

pub struct Snapshot<'r, 's, R: ResourceProvider<'s, S>, S : Storage + 's> {
    snapshot : RefCell<HashMap<Url, S::Handle<'s>>>,
    resources: &'r R
}

impl<'r, 's, R, S> Snapshot<'r, 's, R, S> 
        where R: ResourceProvider<'s, S>, S: Storage + 's {
    
    pub fn new(resources: &'r R) -> Self {
        Self { snapshot: RefCell::new(HashMap::new()), resources }
    }
}

#[async_trait(?Send)]
impl<'r, 's, R: ResourceProvider<'s, S>, S: Storage + 's> ResourceProvider<'s, S> for Snapshot<'r, 's, R, S> {
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
    store: &'s S
}

#[async_trait(?Send)]
impl<'s, S: Storage + 's> ResourceProvider<'s, S> for HttpProvider<'s, S> {
    async fn retrieve(&self, res: &Url) -> Result<S::Handle<'s>, Error> {
        if res.scheme() != "http" && res.scheme() != "https" {
            return Err(Error::new_const(ErrorKind::NotFound, "Only supports file:// scheme"))
        }
        let res = surf::get(res);
        let bytes = res.recv_bytes().await.map_err(|_| Error::new("Unable to fetch"))?;
        let val = Value::Buffer(Bytes::from(bytes));
        self.store.insert_from(&val)
    }
}

pub struct BuiltinsProvider<'s, S: Storage + 's> {
    store: &'s S,
    handle: RefCell<Option<S::Handle<'s>>>
}

impl<'s, S: Storage + 's> BuiltinsProvider<'s, S> {
    async fn prelude(&self) -> Result<S::Handle<'s>, Error> {
        {
            let opt = self.handle.borrow();
            if let Some(h) = opt.deref() {
                return Ok(h.clone())
            }
        }
        let mut opt = self.handle.borrow_mut();
        // we need to compile the prelude
        let prelude = crate::core::prelude::PRELUDE;
        let lexer = crate::parse::Lexer::new(prelude);
        let parser = crate::grammar::ModuleParser::new();
        let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
        let expr = module.transpile();
        let compiled = expr.compile(self.store, &Env::new())?.store_in(self.store)?;
        *opt = Some(compiled.clone());
        Ok(compiled)
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