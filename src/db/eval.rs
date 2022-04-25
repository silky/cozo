use std::collections::BTreeMap;
use cozorocks::{SlicePtr, StatusCode};
use crate::db::engine::{Session};
use crate::relation::table::{Table};
use crate::relation::tuple::{OwnTuple, Tuple};
use crate::relation::typing::Typing;
use crate::relation::value::Value;
use crate::error::{CozoError, Result};
use crate::relation::data::DataKind;

pub trait Environment<T: AsRef<[u8]>> {
    fn get_stack_depth(&self) -> i32;
    fn push_env(&mut self);
    fn pop_env(&mut self) -> Result<()>;
    fn define_variable(&mut self, name: &str, val: &Value, in_root: bool) -> Result<()> {
        let mut data = Tuple::with_data_prefix(DataKind::Value);
        data.push_value(val);
        self.define_data(name, data, in_root)
    }

    fn define_type_alias(&mut self, name: &str, typ: &Typing, in_root: bool) -> Result<()> {
        let mut data = Tuple::with_data_prefix(DataKind::TypeAlias);
        data.push_str(typ.to_string());
        self.define_data(name, data, in_root)
    }

    fn define_table(&mut self, table: &Table, in_root: bool) -> Result<()> {
        todo!()
    }
    fn resolve(&self, name: &str) -> Result<Option<Tuple<T>>>;
    fn delete_defined(&mut self, name: &str, in_root: bool) -> Result<()>;
    fn define_data(&mut self, name: &str, data: OwnTuple, in_root: bool) -> Result<()>;
    fn encode_definable_key(&self, name: &str, in_root: bool) -> OwnTuple {
        let depth_code = if in_root { 0 } else { self.get_stack_depth() as i64 };
        let mut tuple = Tuple::with_null_prefix();
        tuple.push_str(name);
        tuple.push_int(depth_code);
        tuple
    }
}

pub struct MemoryEnv {
    root: BTreeMap<String, OwnTuple>,
    stack: Vec<BTreeMap<String, OwnTuple>>,
}

impl Default for MemoryEnv {
    fn default() -> Self {
        MemoryEnv { root: BTreeMap::default(), stack: vec![BTreeMap::default()] }
    }
}

impl Environment<Vec<u8>> for MemoryEnv {
    fn get_stack_depth(&self) -> i32 {
        -(self.stack.len() as i32)
    }

    fn push_env(&mut self) {
        self.stack.push(BTreeMap::default());
    }

    fn pop_env(&mut self) -> Result<()> {
        if self.stack.len() > 1 {
            self.stack.pop();
        }
        Ok(())
    }

    fn resolve(&self, name: &str) -> Result<Option<OwnTuple>> {
        for layer in self.stack.iter() {
            if let Some(res) = layer.get(name) {
                return Ok(Some(res.clone()))
            }
        }
        Ok(self.root.get(name).cloned())
    }

    fn delete_defined(&mut self, name: &str, in_root: bool) -> Result<()> {
        if in_root {
            self.root.remove(name);
        } else {
            for layer in self.stack.iter_mut().rev() {
                if let Some(_) = layer.remove(name) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn define_data(&mut self, name: &str, data: OwnTuple, in_root: bool) -> Result<()> {
        if in_root {
            self.root.insert(name.to_string(), data);
        } else {
            let last = self.stack.last_mut().unwrap();
            last.insert(name.to_string(), data);
        }
        Ok(())
    }
}


impl<'a> Environment<SlicePtr> for Session<'a> {
    fn get_stack_depth(&self) -> i32 {
        self.stack_depth
    }

    fn push_env(&mut self) {
        self.stack_depth -= 1;
    }

    fn pop_env(&mut self) -> Result<()> {
        // Remove all stuff starting with the stack depth from the temp session
        let mut prefix = Tuple::with_null_prefix();
        prefix.push_int(self.stack_depth as i64);
        let it = self.txn.iterator(false, &self.temp_cf);
        it.seek(&prefix);
        for val in it.keys() {
            let cur = Tuple::new(val);
            if cur.starts_with(&prefix) {
                if let Some(name) = cur.get(1) {
                    let mut ikey = Tuple::with_null_prefix();
                    ikey.push_value(&name);
                    ikey.push_int(self.stack_depth as i64);

                    self.txn.del(false, &self.temp_cf, cur)?;
                    self.txn.del(false, &self.temp_cf, ikey)?;
                }
            } else {
                break;
            }
        }

        if self.stack_depth != 0 {
            self.stack_depth += 1;
        }
        Ok(())
    }

    fn resolve(&self, name: &str) -> Result<Option<Tuple<SlicePtr>>> {
        let mut tuple = Tuple::with_null_prefix();
        tuple.push_str(name);
        let it = self.txn.iterator(false, &self.temp_cf);
        it.seek(&tuple);
        if let Some((tk, vk)) = it.pair() {
            let k = Tuple::new(tk);
            if k.starts_with(&tuple) {
                return Ok(Some(Tuple::new(vk)));
            }
        }
        let root_key = self.encode_definable_key(name, true);
        match self.txn.get(true, &self.perm_cf, root_key) {
            Ok(root_res) => Ok(Some(Tuple::new(root_res))),
            Err(e) if e.status.code == StatusCode::kNotFound => Ok(None),
            Err(e) => Err(CozoError::Bridge(e))
        }
    }

    fn delete_defined(&mut self, name: &str, in_root: bool) -> Result<()> {
        let key = self.encode_definable_key(name, in_root);
        if in_root {
            self.txn.del(true, &self.perm_cf, key)?;
        } else {
            let it = self.txn.iterator(false, &self.temp_cf);
            it.seek(&key);
            if let Some(found_key) = it.key() {
                let found_key_tuple = Tuple::new(found_key);
                if found_key_tuple.starts_with(&key) {
                    let mut ikey = Tuple::with_null_prefix();
                    ikey.push_value(&found_key_tuple.get(1).unwrap());
                    ikey.push_value(&found_key_tuple.get(0).unwrap());
                    self.txn.del(false, &self.temp_cf, found_key_tuple)?;
                    self.txn.del(false, &self.temp_cf, ikey)?;
                }
            }
        }
        // TODO cleanup if the thing deleted is a table

        Ok(())
    }

    fn define_data(&mut self, name: &str, data: OwnTuple, in_root: bool) -> Result<()> {
        let key = self.encode_definable_key(name, in_root);
        if in_root {
            self.txn.put(true, &self.perm_cf, key, data)?;
        } else {
            let mut ikey = Tuple::with_null_prefix();
            ikey.push_int(self.stack_depth as i64);
            ikey.push_str(name);
            self.txn.put(false, &self.temp_cf, key, data)?;
            self.txn.put(false, &self.temp_cf, ikey, "")?;
        }
        Ok(())
    }
}