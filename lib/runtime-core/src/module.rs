use crate::{
    backend::{Backend, FuncResolver, ProtectedCaller},
    cache::{Cache, Error as CacheError},
    error,
    import::ImportObject,
    structures::{Map, TypedIndex},
    typed_func::EARLY_TRAPPER,
    types::{
        FuncIndex, FuncSig, GlobalDescriptor, GlobalIndex, GlobalInit, ImportedFuncIndex,
        ImportedGlobalIndex, ImportedMemoryIndex, ImportedTableIndex, Initializer,
        LocalGlobalIndex, LocalMemoryIndex, LocalTableIndex, MemoryDescriptor, MemoryIndex,
        SigIndex, TableDescriptor, TableIndex,
    },
    Instance,
};

use crate::backend::CacheGen;
use hashbrown::HashMap;
use indexmap::IndexMap;
use std::sync::Arc;

/// This is used to instantiate a new WebAssembly module.
#[doc(hidden)]
pub struct ModuleInner {
    pub func_resolver: Box<dyn FuncResolver>,
    pub protected_caller: Box<dyn ProtectedCaller>,

    pub cache_gen: Box<dyn CacheGen>,

    pub info: ModuleInfo,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModuleInfo {
    // This are strictly local and the typsystem ensures that.
    pub memories: Map<LocalMemoryIndex, MemoryDescriptor>,
    pub globals: Map<LocalGlobalIndex, GlobalInit>,
    pub tables: Map<LocalTableIndex, TableDescriptor>,

    // These are strictly imported and the typesystem ensures that.
    pub imported_functions: Map<ImportedFuncIndex, ImportName>,
    pub imported_memories: Map<ImportedMemoryIndex, (ImportName, MemoryDescriptor)>,
    pub imported_tables: Map<ImportedTableIndex, (ImportName, TableDescriptor)>,
    pub imported_globals: Map<ImportedGlobalIndex, (ImportName, GlobalDescriptor)>,

    pub exports: HashMap<String, ExportIndex>,

    pub data_initializers: Vec<DataInitializer>,
    pub elem_initializers: Vec<TableInitializer>,

    pub start_func: Option<FuncIndex>,

    pub func_assoc: Map<FuncIndex, SigIndex>,
    pub signatures: Map<SigIndex, Arc<FuncSig>>,
    pub backend: Backend,

    pub namespace_table: StringTable<NamespaceIndex>,
    pub name_table: StringTable<NameIndex>,

    pub wasm_hash: WasmHash,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WasmHash([u8; 32]);

impl WasmHash {
    pub fn generate(wasm: &[u8]) -> Self {
        WasmHash(crate::cache::hash_data(wasm))
    }

    pub(crate) fn into_array(self) -> [u8; 32] {
        self.0
    }
}

/// A compiled WebAssembly module.
///
/// `Module` is returned by the [`compile`] and
/// [`compile_with`] functions.
///
/// [`compile`]: fn.compile.html
/// [`compile_with`]: fn.compile_with.html
pub struct Module {
    inner: Arc<ModuleInner>,
}

impl Module {
    pub(crate) fn new(inner: Arc<ModuleInner>) -> Self {
        unsafe {
            EARLY_TRAPPER
                .with(|ucell| *ucell.get() = Some(inner.protected_caller.get_early_trapper()));
        }
        Module { inner }
    }

    /// Instantiate a WebAssembly module with the provided [`ImportObject`].
    ///
    /// [`ImportObject`]: struct.ImportObject.html
    ///
    /// # Note:
    /// Instantiating a `Module` will also call the function designated as `start`
    /// in the WebAssembly module, if there is one.
    ///
    /// # Usage:
    /// ```
    /// # use wasmer_runtime_core::error::Result;
    /// # use wasmer_runtime_core::Module;
    /// # use wasmer_runtime_core::imports;
    /// # fn instantiate(module: &Module) -> Result<()> {
    /// let import_object = imports! {
    ///     // ...
    /// };
    /// let instance = module.instantiate(&import_object)?;
    /// // ...
    /// # Ok(())
    /// # }
    /// ```
    pub fn instantiate(&self, import_object: &ImportObject) -> error::Result<Instance> {
        Instance::new(Arc::clone(&self.inner), import_object)
    }

    pub fn cache(&self) -> Result<Cache, CacheError> {
        let (info, backend_metadata, code) = self.inner.cache_gen.generate_cache(&self.inner)?;
        Ok(Cache::from_parts(info, backend_metadata, code))
    }

    pub fn info(&self) -> &ModuleInfo {
        &self.inner.info
    }
}

impl ModuleInner {}

#[doc(hidden)]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImportName {
    pub namespace_index: NamespaceIndex,
    pub name_index: NameIndex,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportIndex {
    Func(FuncIndex),
    Memory(MemoryIndex),
    Global(GlobalIndex),
    Table(TableIndex),
}

/// A data initializer for linear memory.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DataInitializer {
    /// The index of the memory to initialize.
    pub memory_index: MemoryIndex,
    /// Either a constant offset or a `get_global`
    pub base: Initializer,
    /// The initialization data.
    #[cfg_attr(feature = "cache", serde(with = "serde_bytes"))]
    pub data: Vec<u8>,
}

/// A WebAssembly table initializer.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TableInitializer {
    /// The index of a table to initialize.
    pub table_index: TableIndex,
    /// Either a constant offset or a `get_global`
    pub base: Initializer,
    /// The values to write into the table elements.
    pub elements: Vec<FuncIndex>,
}

pub struct StringTableBuilder<K: TypedIndex> {
    map: IndexMap<String, (K, u32, u32)>,
    buffer: String,
    count: u32,
}

impl<K: TypedIndex> StringTableBuilder<K> {
    pub fn new() -> Self {
        Self {
            map: IndexMap::new(),
            buffer: String::new(),
            count: 0,
        }
    }

    pub fn register<S>(&mut self, s: S) -> K
    where
        S: Into<String> + AsRef<str>,
    {
        let s_str = s.as_ref();

        if self.map.contains_key(s_str) {
            self.map[s_str].0
        } else {
            let offset = self.buffer.len();
            let length = s_str.len();
            let index = TypedIndex::new(self.count as _);

            self.buffer.push_str(s_str);
            self.map
                .insert(s.into(), (index, offset as u32, length as u32));
            self.count += 1;

            index
        }
    }

    pub fn finish(self) -> StringTable<K> {
        let table = self
            .map
            .values()
            .map(|(_, offset, length)| (*offset, *length))
            .collect();

        StringTable {
            table,
            buffer: self.buffer,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StringTable<K: TypedIndex> {
    table: Map<K, (u32, u32)>,
    buffer: String,
}

impl<K: TypedIndex> StringTable<K> {
    pub fn new() -> Self {
        Self {
            table: Map::new(),
            buffer: String::new(),
        }
    }

    pub fn get(&self, index: K) -> &str {
        let (offset, length) = self.table[index];
        let offset = offset as usize;
        let length = length as usize;

        &self.buffer[offset..offset + length]
    }
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct NamespaceIndex(u32);

impl TypedIndex for NamespaceIndex {
    #[doc(hidden)]
    fn new(index: usize) -> Self {
        NamespaceIndex(index as _)
    }

    #[doc(hidden)]
    fn index(&self) -> usize {
        self.0 as usize
    }
}

#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct NameIndex(u32);

impl TypedIndex for NameIndex {
    #[doc(hidden)]
    fn new(index: usize) -> Self {
        NameIndex(index as _)
    }

    #[doc(hidden)]
    fn index(&self) -> usize {
        self.0 as usize
    }
}
