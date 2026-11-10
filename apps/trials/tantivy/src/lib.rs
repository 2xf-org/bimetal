//! Tantivy, the full-text search engine, behind Bimetal: build an index and
//! search it from Python, from Node.js, and from a browser.

use std::sync::Mutex;

use bimetal::Model;
use serde::{Deserialize, Serialize};
use tantivy::collector::{Count, TopDocs};
use tantivy::indexer::SingleSegmentIndexWriter;
use tantivy::query::QueryParser;
use tantivy::schema::document::Document;
use tantivy::schema::{
    FAST, Field, FieldType, INDEXED, STORED, Schema, SchemaBuilder, TEXT, TantivyDocument,
};
use tantivy::{IndexBuilder, IndexReader, IndexSettings, IndexWriter, ReloadPolicy, Term};

/// What a field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Model)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Tokenized text, which queries match by its words.
    Text,
    /// A 64-bit unsigned integer.
    Integer,
    /// A 64-bit float.
    Float,
}

/// One field of the documents of an index.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct FieldSpec {
    /// The field's name.
    #[bimetal(min_length = 1)]
    pub name: String,
    /// What the field holds.
    pub kind: FieldKind,
    /// Whether a search can return the field's value.
    #[serde(default)]
    pub stored: bool,
}

/// One document that a search found.
#[derive(Debug, Clone, Serialize, Deserialize, Model)]
pub struct Hit {
    /// How well the document matches.
    pub score: f32,
    /// The stored fields, each with its list of values.
    pub document: serde_json::Value,
}

/// Why an index operation failed.
#[derive(Debug, thiserror::Error, bimetal::Error)]
pub enum SearchError {
    /// The index itself failed.
    #[error("index error: {reason}")]
    #[bimetal(details)]
    Index {
        /// What Tantivy reported.
        reason: String,
    },
    /// The query does not parse.
    #[error("invalid query: {reason}")]
    #[bimetal(details)]
    Query {
        /// What the parser reported.
        reason: String,
    },
    /// A document does not fit the schema.
    #[error("invalid document: {reason}")]
    #[bimetal(details)]
    Document {
        /// What the parser reported.
        reason: String,
    },
    /// A field name is not in the schema.
    #[error("no field named {name}")]
    #[bimetal(details)]
    UnknownField {
        /// The name asked for.
        name: String,
    },
}

impl From<tantivy::TantivyError> for SearchError {
    fn from(error: tantivy::TantivyError) -> Self {
        Self::Index {
            reason: error.to_string(),
        }
    }
}

/// What opening a directory fails with in a browser.
#[cfg(target_arch = "wasm32")]
fn unsupported(path: &str, _fields: &[FieldSpec]) -> SearchError {
    SearchError::Index {
        reason: format!("a browser cannot open the directory {path}"),
    }
}

fn schema_of(fields: &[FieldSpec]) -> Schema {
    let mut builder = SchemaBuilder::new();
    for field in fields {
        match (field.kind, field.stored) {
            (FieldKind::Text, true) => builder.add_text_field(&field.name, TEXT | STORED),
            (FieldKind::Text, false) => builder.add_text_field(&field.name, TEXT),
            (FieldKind::Integer, true) => {
                builder.add_u64_field(&field.name, INDEXED | FAST | STORED)
            }
            (FieldKind::Integer, false) => builder.add_u64_field(&field.name, INDEXED | FAST),
            (FieldKind::Float, true) => builder.add_f64_field(&field.name, INDEXED | FAST | STORED),
            (FieldKind::Float, false) => builder.add_f64_field(&field.name, INDEXED | FAST),
        };
    }
    builder.build()
}

fn parse_document(
    schema: &Schema,
    document: &serde_json::Value,
) -> Result<TantivyDocument, SearchError> {
    TantivyDocument::parse_json(schema, &document.to_string()).map_err(|error| {
        SearchError::Document {
            reason: error.to_string(),
        }
    })
}

fn field_of(schema: &Schema, name: &str) -> Result<Field, SearchError> {
    schema
        .get_field(name)
        .map_err(|_| SearchError::UnknownField {
            name: name.to_owned(),
        })
}

/// A search index, in memory or in a directory.
pub struct Index {
    index: tantivy::Index,
}

#[bimetal::export]
impl Index {
    /// Create an empty index in memory.
    #[bimetal(constructor)]
    pub fn new(fields: Vec<FieldSpec>) -> Result<Self, SearchError> {
        Ok(Self {
            index: tantivy::Index::create_in_ram(schema_of(&fields)),
        })
    }

    /// Create an empty index in a directory. A browser has no directories.
    #[bimetal(constructor)]
    pub fn create(path: &str, fields: Vec<FieldSpec>) -> Result<Self, SearchError> {
        #[cfg(target_arch = "wasm32")]
        return Err(unsupported(path, &fields));
        #[cfg(not(target_arch = "wasm32"))]
        Ok(Self {
            index: tantivy::Index::create_in_dir(path, schema_of(&fields))?,
        })
    }

    /// Open the index in a directory. A browser has no directories.
    #[bimetal(constructor)]
    pub fn open(path: &str) -> Result<Self, SearchError> {
        #[cfg(target_arch = "wasm32")]
        return Err(unsupported(path, &[]));
        #[cfg(not(target_arch = "wasm32"))]
        Ok(Self {
            index: tantivy::Index::open_in_dir(path)?,
        })
    }

    /// The fields of the documents.
    #[bimetal(getter)]
    pub fn fields(&self) -> Vec<FieldSpec> {
        let schema = self.index.schema();
        schema
            .fields()
            .map(|(_, entry)| FieldSpec {
                name: entry.name().to_owned(),
                kind: match entry.field_type() {
                    FieldType::U64(_) => FieldKind::Integer,
                    FieldType::F64(_) => FieldKind::Float,
                    _ => FieldKind::Text,
                },
                stored: entry.is_stored(),
            })
            .collect()
    }

    /// Open a writer, which commits when a `with` block ends and rolls back
    /// when one raises. It needs threads, which a browser does not have, so
    /// use [`BulkWriter`] there.
    #[bimetal(returns(resource))]
    pub fn writer(
        &self,
        #[bimetal(default = 50000000)] heap_bytes: u64,
    ) -> Result<Writer, SearchError> {
        Ok(Writer {
            writer: self.index.writer(heap_bytes as usize)?,
            schema: self.index.schema(),
        })
    }

    /// Open a searcher over what is committed now.
    #[bimetal(returns(resource))]
    pub fn searcher(&self) -> Result<Searcher, SearchError> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Searcher {
            index: self.index.clone(),
            reader,
        })
    }
}

/// Adds documents to an index, on background threads.
pub struct Writer {
    writer: IndexWriter,
    schema: Schema,
}

#[bimetal::export]
impl Writer {
    /// Add a document, a JSON object of field names and values.
    pub fn add(&mut self, document: serde_json::Value) -> Result<u64, SearchError> {
        Ok(self
            .writer
            .add_document(parse_document(&self.schema, &document)?)?)
    }

    /// Delete the documents whose text field holds the word `value`.
    pub fn delete(&mut self, field: &str, value: &str) -> Result<u64, SearchError> {
        let field = field_of(&self.schema, field)?;
        Ok(self.writer.delete_term(Term::from_field_text(field, value)))
    }

    /// Make what was added visible to new searchers.
    pub fn commit(&mut self) -> Result<u64, SearchError> {
        Ok(self.writer.commit()?)
    }

    /// Forget everything since the last commit.
    pub fn rollback(&mut self) -> Result<u64, SearchError> {
        Ok(self.writer.rollback()?)
    }

    /// End a `with` block: commit if it finished, roll back if it failed.
    #[bimetal(exit)]
    pub fn finish(&mut self, failed: bool) -> Result<(), SearchError> {
        if failed {
            self.writer.rollback()?;
        } else {
            self.writer.commit()?;
        }
        Ok(())
    }
}

/// Builds one index on the calling thread, which also works in a browser.
pub struct BulkWriter {
    /// The writer is `Send`, but not `Sync`, which a resource must be. A
    /// `Mutex` makes it `Sync`, and `&mut self` reaches it without locking.
    writer: Mutex<SingleSegmentIndexWriter>,
    schema: Schema,
}

#[bimetal::export]
impl BulkWriter {
    /// Start an index in memory.
    #[bimetal(constructor)]
    pub fn new(
        fields: Vec<FieldSpec>,
        #[bimetal(default = 20000000)] heap_bytes: u64,
    ) -> Result<Self, SearchError> {
        let schema = schema_of(&fields);
        let settings = IndexSettings {
            docstore_compress_dedicated_thread: false,
            ..IndexSettings::default()
        };
        let writer = IndexBuilder::new()
            .schema(schema.clone())
            .settings(settings)
            .single_segment_index_writer(
                tantivy::directory::RamDirectory::default(),
                heap_bytes as usize,
            )?;
        Ok(Self {
            writer: Mutex::new(writer),
            schema,
        })
    }

    /// Add a document, a JSON object of field names and values.
    pub fn add(&mut self, document: serde_json::Value) -> Result<(), SearchError> {
        let document = parse_document(&self.schema, &document)?;
        let writer = self
            .writer
            .get_mut()
            .expect("nothing panics holding the writer");
        Ok(writer.add_document(document)?)
    }

    /// Finish the index. The writer is closed afterwards.
    #[bimetal(returns(resource))]
    pub fn finish(self) -> Result<Index, SearchError> {
        let writer = self
            .writer
            .into_inner()
            .expect("nothing panics holding the writer");
        Ok(Index {
            index: writer.finalize()?,
        })
    }
}

/// Searches what an index had committed when the searcher opened, or when
/// `reload` last ran.
pub struct Searcher {
    index: tantivy::Index,
    reader: IndexReader,
}

#[bimetal::export]
impl Searcher {
    /// How many documents the searcher sees.
    #[bimetal(len)]
    pub fn num_documents(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// See what was committed since.
    pub fn reload(&mut self) -> Result<(), SearchError> {
        Ok(self.reader.reload()?)
    }

    /// Return the best matches of a query over `fields`, or over every text
    /// field.
    pub fn search(
        &self,
        query: &str,
        fields: Option<Vec<String>>,
        #[bimetal(default = 10)] limit: u32,
    ) -> Result<Vec<Hit>, SearchError> {
        let searcher = self.reader.searcher();
        let schema = self.index.schema();
        let query = self.parse(&schema, query, fields)?;
        let top = searcher.search(
            &query,
            &TopDocs::with_limit(limit as usize).order_by_score(),
        )?;
        top.into_iter()
            .map(|(score, address)| {
                let document: TantivyDocument = searcher.doc(address)?;
                let document = serde_json::from_str(&document.to_json(&schema))
                    .expect("Tantivy writes valid JSON");
                Ok(Hit { score, document })
            })
            .collect()
    }

    /// How many documents match a query.
    pub fn count(&self, query: &str) -> Result<u64, SearchError> {
        let schema = self.index.schema();
        let query = self.parse(&schema, query, None)?;
        Ok(self.reader.searcher().search(&query, &Count)? as u64)
    }

    /// Walk every match of a query, best first, without holding them all.
    #[bimetal(returns(resource))]
    pub fn hits(
        &self,
        query: &str,
        #[bimetal(default = 100)] page_size: u32,
    ) -> Result<Hits, SearchError> {
        let schema = self.index.schema();
        let query = self.parse(&schema, query, None)?;
        Ok(Hits {
            searcher: self.reader.searcher(),
            schema,
            query,
            page_size: page_size as usize,
            offset: 0,
            page: Vec::new().into_iter(),
        })
    }
}

impl Searcher {
    fn parse(
        &self,
        schema: &Schema,
        query: &str,
        fields: Option<Vec<String>>,
    ) -> Result<Box<dyn tantivy::query::Query>, SearchError> {
        let defaults = match fields {
            Some(names) => names
                .iter()
                .map(|name| field_of(schema, name))
                .collect::<Result<Vec<Field>, SearchError>>()?,
            None => schema
                .fields()
                .filter(|(_, entry)| matches!(entry.field_type(), FieldType::Str(_)))
                .map(|(field, _)| field)
                .collect(),
        };
        QueryParser::for_index(&self.index, defaults)
            .parse_query(query)
            .map_err(|error| SearchError::Query {
                reason: error.to_string(),
            })
    }
}

/// The matches of one query, fetched a page at a time.
pub struct Hits {
    searcher: tantivy::Searcher,
    schema: Schema,
    query: Box<dyn tantivy::query::Query>,
    page_size: usize,
    offset: usize,
    page: std::vec::IntoIter<Hit>,
}

#[bimetal::export]
impl Hits {
    /// The next match, or nothing at the end.
    #[bimetal(next)]
    pub fn next_hit(&mut self) -> Result<Option<Hit>, SearchError> {
        if let Some(hit) = self.page.next() {
            return Ok(Some(hit));
        }
        let collector = TopDocs::with_limit(self.page_size)
            .and_offset(self.offset)
            .order_by_score();
        let found = self.searcher.search(&self.query, &collector)?;
        self.offset += found.len();
        let hits: Result<Vec<Hit>, SearchError> = found
            .into_iter()
            .map(|(score, address)| {
                let document: TantivyDocument = self.searcher.doc(address)?;
                let document = serde_json::from_str(&document.to_json(&self.schema))
                    .expect("Tantivy writes valid JSON");
                Ok(Hit { score, document })
            })
            .collect();
        self.page = hits?.into_iter();
        Ok(self.page.next())
    }
}

/// Return the first value of a stored text field of a hit's document.
#[bimetal::export]
#[must_use]
pub fn first_text(document: serde_json::Value, field: &str) -> Option<String> {
    document
        .get(field)?
        .as_array()?
        .first()?
        .as_str()
        .map(str::to_owned)
}
