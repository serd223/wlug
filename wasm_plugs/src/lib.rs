use std::{collections::HashMap, path::Path};

use wasmtime::{
    Engine, Extern, Func, Instance, IntoFunc, Linker, Module, Store, TypedFunc, UnknownImportError,
    Val, ValType, WasmParams, WasmResults,
};

const DEPS_EXPORT: &str = "__deps";
const INIT_EXPORT: &str = "__init";

pub type PlugId = usize;

pub struct PlugContext<T>(pub PlugId, pub T);

pub struct Plug<T> {
    pub id: PlugId,
    pub module: Module,
    pub linker: Linker<PlugContext<T>>,
    pub instance: Option<Instance>,
    pub deps: Vec<String>,
    pub exports: Vec<String>,
    pub imports: Vec<String>,
}

pub struct PlugMetadata {
    pub deps: Vec<String>,
    pub exports: Vec<String>,
    pub imports: Vec<String>,
}

pub struct PlugsHostFns {
    pub fns: Vec<(String, Extern)>,
}

pub struct Plugs<T> {
    pub store: Store<PlugContext<T>>,
    pub items: HashMap<String, Plug<T>>,
    pub order: Vec<String>,
    pub host_fns: PlugsHostFns,
}

impl<T> Plugs<T> {
    /// Create a new `Plugs` with a `wasmtime::Engine` and state
    pub fn new(engine: &Engine, state: T) -> Self {
        Self {
            store: Store::new(engine, PlugContext(0, state)),
            items: HashMap::new(),
            order: Vec::new(),
            host_fns: PlugsHostFns { fns: Vec::new() },
        }
    }

    pub fn add_host_fn<Params, Results>(
        &mut self,
        name: String,
        func: impl IntoFunc<PlugContext<T>, Params, Results>,
    ) {
        let func = Func::wrap(&mut self.store, func);
        let func = Into::<Extern>::into(func);
        self.host_fns.fns.push((name, func));
    }

    pub fn link_host(&mut self, linker: &mut Linker<PlugContext<T>>) -> wasmtime::Result<()> {
        for (name, func) in self.host_fns.fns.iter() {
            linker.define(&mut self.store, "env", name, func.clone())?;
        }
        Ok(())
    }

    /// Extract metadata from the specified module by instantiating a temporary instance and running the
    /// necessary reserved functions (such as `deps`) for metadata extraction.
    pub fn extract_metadata(
        &mut self,
        engine: &Engine,
        module: &Module,
    ) -> wasmtime::Result<PlugMetadata> {
        let mut linker = Linker::new(engine);

        let mut imports = Vec::new();
        let instance = loop {
            match linker.instantiate(&mut self.store, &module) {
                Ok(inst) => break inst,
                Err(e) => {
                    let e: UnknownImportError = e.downcast()?;
                    let ftype = e.ty().func().unwrap().clone();
                    let result_types = ftype.results().collect::<Vec<_>>();
                    linker.func_new("env", e.name(), ftype, move |_, _, results| {
                        for (i, res_type) in result_types.iter().enumerate() {
                            results[i] = match res_type {
                                ValType::I32 => Val::I32(0),
                                ValType::I64 => Val::I64(0),
                                ValType::F32 => Val::F32(0f32.to_bits()),
                                ValType::F64 => Val::F64(0f64.to_bits()),
                                ValType::V128 => Val::V128(0u128.into()),
                                ValType::Ref(r) => Val::null_ref(r.heap_type()),
                            };
                        }

                        Ok(())
                    })?;
                    let imp = e.name().to_string();
                    let is_host_fn = self.host_fns.fns.iter().any(|(n, _)| imp.eq(n));
                    if !is_host_fn {
                        imports.push(e.name().to_string());
                    }
                    continue;
                }
            }
        };

        // TODO: The plugin name could also be extracted in a similar way instead of
        // relying on the file name. The current file name approach makes the system simpler
        // but I think I will switch to a `name` export in the future.

        // Extract dependencies (optional)
        let mut deps = Vec::new();
        if let Ok(deps_fn) = instance.get_typed_func::<(), u32>(&mut self.store, DEPS_EXPORT) {
            let mut deps_ptr = deps_fn.call(&mut self.store, ())?;
            let memory = {
                if let Some(m) = instance.get_memory(&mut self.store, "memory") {
                    m
                } else {
                    return Err(wasmtime::Error::msg("Couldn't find 'memory' export"));
                }
            };
            let mut deps_buf = vec![0u8];
            deps.push(String::new());
            memory.read(&mut self.store, deps_ptr as usize, &mut deps_buf)?;
            while deps_buf[0] != 0 {
                let c = deps_buf[0] as char;
                if c == ';' {
                    deps.push(String::new());
                } else {
                    deps.last_mut().unwrap().push(c);
                }
                deps_ptr += 1;
                memory.read(&mut self.store, deps_ptr as usize, &mut deps_buf)?;
            }
        }
        let exports = module.exports().map(|e| e.name().to_string()).collect();
        Ok(PlugMetadata {
            deps,
            exports,
            imports,
        })
    }

    /// Add plug (without linking except host functions)
    pub fn add(&mut self, file_path: &str, engine: &Engine) -> wasmtime::Result<()> {
        let fp = Path::new(file_path);
        let ext = fp.extension().unwrap();
        let ext_len = ext.len();
        let name = fp.file_name().unwrap().to_str().unwrap();
        let len = name.len();
        let name = &name[..len - ext_len - 1];
        let module = Module::from_file(engine, file_path)?;

        let metadata = self.extract_metadata(engine, &module)?;

        let mut linker = Linker::new(engine);

        // Link host functions
        self.link_host(&mut linker)?;

        self.items.insert(
            name.to_string(),
            Plug {
                id: self.order.len(),
                module,
                linker,
                instance: None,
                deps: metadata.deps,
                exports: metadata.exports,
                imports: metadata.imports,
            },
        );
        self.order.push(name.to_string());

        Ok(())
    }

    /// Link all plugs, load order is important (TODO: auto sorting)
    /// and circular dependencies are disallowed (won't change, TODO: report as error)
    pub fn link(&mut self) -> wasmtime::Result<()> {
        // TODO: perhaps sort the plugins before linking them so that all plugins are guaranteed to be loaded after their dependencies
        // this could also be a chance for us to detect circular dependencies and throw an error in that case since they are disallowed
        //
        // Circular dependencies are disallowed because we can't easily detect which _symbol_ depends on which, we only know which plugin
        // depends on which symbols and that isn't really enough to properly resolve all cases. If we were to just use that info, there
        // could be some edge case where the linker doesn't properly link everything especially if the dependency graph is very
        // convoluted and the circular dependency is deep within the dependency tree.
        for name in self.order.iter() {
            let p = self.items.get_mut(name.as_str()).unwrap();
            let deps = p.deps.clone();
            let mut imports = p.imports.clone();
            let mut to_import = Vec::new();

            #[cfg(debug_assertions)]
            println!("\n[Plugs::link]: '{name}' has {deps:?} as dependencies");

            if imports.len() > 0 {
                for dep_name in deps.iter() {
                    if let Some(p_dep) = self.items.get_mut(dep_name) {
                        imports = {
                            let mut res = Vec::new();
                            for imp in imports {
                                let exists = p_dep.exports.contains(&imp);
                                if exists {
                                    let inst = if let Some(inst) = &p_dep.instance {
                                        inst
                                    } else {
                                        return Err(wasmtime::Error::msg(format!("Dependency '{dep_name}' in plugin '{name}' hasn't been instantiated yet")));
                                    };

                                    let export = if let Some(e) =
                                        inst.get_export(&mut self.store, &imp)
                                    {
                                        e
                                    } else {
                                        return Err(wasmtime::Error::msg(format!("Dependency '{dep_name}' doesn't have export '{imp}' required by plugin '{name}'")));
                                    };

                                    #[cfg(debug_assertions)]
                                    println!("[Plugs::link]: Will define '{imp}' from '{dep_name}' in '{name}'");

                                    to_import.push((imp, export));
                                } else {
                                    res.push(imp);
                                }
                            }

                            res
                        };
                    } else {
                        return Err(wasmtime::Error::msg(format!(
                            "'{dep_name}' is not a valid dependency"
                        )));
                    }
                }
            }

            let p = self.items.get_mut(name.as_str()).unwrap();

            if imports.len() > 0 {
                return Err(wasmtime::Error::msg(format!(
                    "Plugin '{name}' has unresolved imports: {:?}",
                    imports
                )));
            }

            for (imp, export) in to_import {
                p.linker.define(&mut self.store, "env", &imp, export)?;
            }

            p.instance = Some(p.linker.instantiate(&mut self.store, &p.module)?);
        }
        Ok(())
    }

    pub fn init(&mut self) -> wasmtime::Result<()> {
        let names = self.order.clone();

        for name in names {
            if let Ok((id, init_fn)) = self.get_func_with_id::<(), ()>(&name, INIT_EXPORT) {
                self.set_current_id(id);
                init_fn.call(&mut self.store, ())?;
            }
        }

        Ok(())
    }

    /// Convenience function calling function in a plugin and setting the plugin's id as the current
    pub fn call<P: WasmParams, R: WasmResults>(
        &mut self,
        plug: &str,
        func: &str,
        params: P,
    ) -> wasmtime::Result<R> {
        let (id, f) = self.get_func_with_id(plug, func)?;
        self.set_current_id(id);
        f.call(&mut self.store, params)
    }

    /// Must be set before calling any function
    pub fn set_current_id(&mut self, plugin_id: PlugId) {
        self.store.data_mut().0 = plugin_id;
    }

    /// Gets id of plugin by name
    pub fn get_plug_id(&self, name: &str) -> Option<PlugId> {
        if let Some(p) = self.items.get(name) {
            return Some(p.id);
        }
        None
    }

    /// Looks up a function in the specified plugin and returns the id of the plugin and the function
    pub fn get_func_with_id<P: WasmParams, R: WasmResults>(
        &mut self,
        plug: &str,
        func: &str,
    ) -> wasmtime::Result<(PlugId, TypedFunc<P, R>)> {
        if let Some(p) = self.items.get(plug) {
            if let Some(inst) = &p.instance {
                inst.get_typed_func::<P, R>(&mut self.store, func)
                    .map(|f| (p.id, f))
            } else {
                Err(wasmtime::Error::msg(format!(
                    "Plugin '{plug}' hasn't been instantiated yet"
                )))
            }
        } else {
            Err(wasmtime::Error::msg(format!(
                "Couldn't find function '{func}' in plugin '{plug}'"
            )))
        }
    }

    pub fn get_plug_mut(&mut self, name: &str) -> Option<&mut Plug<T>> {
        self.items.get_mut(name)
    }

    pub fn get_plug(&self, name: &str) -> Option<&Plug<T>> {
        self.items.get(name)
    }
}
