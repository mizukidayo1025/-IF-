//! IDs as small numbers in rows. `Interner` holds no Python objects, so rows stay `Send + Sync`;
//! the PyO3 crate keeps one Python object for each symbol in side tables.

use std::collections::HashMap;
use std::sync::Arc;


#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sym(u32);

impl Sym {
    /// Its position among the interned texts, for side tables kept beside the interner.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Default)]
pub struct Interner {
    symbols: HashMap<Arc<str>, Sym>,
    texts: Vec<Arc<str>>,
}

impl Interner {
    pub fn intern(&mut self, text: &str) -> Sym {
        if let Some(&symbol) = self.symbols.get(text) {
            return symbol;
        }
        let symbol = Sym(u32::try_from(self.texts.len()).expect("under 2**32 distinct IDs"));
        let text: Arc<str> = Arc::from(text);
        self.texts.push(text.clone());
        self.symbols.insert(text, symbol);
        symbol
    }

    pub fn text(&self, symbol: Sym) -> &str {
        &self.texts[symbol.0 as usize]
    }
}
