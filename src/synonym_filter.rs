use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use tantivy::tokenizer::{Token, TokenFilter, TokenStream, Tokenizer};

/// Expands configured one-token synonyms at the original token position.
#[derive(Clone)]
pub(crate) struct SynonymFilter {
    synonyms: Arc<BTreeMap<String, Vec<String>>>,
}

impl SynonymFilter {
    pub(crate) fn new(synonyms: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            synonyms: Arc::new(synonyms),
        }
    }
}

impl TokenFilter for SynonymFilter {
    type Tokenizer<T: Tokenizer> = SynonymTokenizer<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        SynonymTokenizer {
            tokenizer,
            synonyms: self.synonyms,
        }
    }
}

#[derive(Clone)]
pub(crate) struct SynonymTokenizer<T> {
    tokenizer: T,
    synonyms: Arc<BTreeMap<String, Vec<String>>>,
}

impl<T: Tokenizer> Tokenizer for SynonymTokenizer<T> {
    type TokenStream<'a> = SynonymTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        SynonymTokenStream {
            tail: self.tokenizer.token_stream(text),
            synonyms: self.synonyms.clone(),
            pending: VecDeque::new(),
            token: Token::default(),
        }
    }
}

pub(crate) struct SynonymTokenStream<T> {
    tail: T,
    synonyms: Arc<BTreeMap<String, Vec<String>>>,
    pending: VecDeque<Token>,
    token: Token,
}

impl<T: TokenStream> TokenStream for SynonymTokenStream<T> {
    fn advance(&mut self) -> bool {
        if let Some(token) = self.pending.pop_front() {
            self.token = token;
            return true;
        }
        if !self.tail.advance() {
            return false;
        }
        self.token = self.tail.token().clone();
        if let Some(expansions) = self.synonyms.get(&self.token.text) {
            for expansion in expansions {
                let mut synonym = self.token.clone();
                synonym.text.clone_from(expansion);
                self.pending.push_back(synonym);
            }
        }
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}
