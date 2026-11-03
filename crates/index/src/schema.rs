//! Index schema and analyzers.

use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, FAST, INDEXED, STORED, STRING,
};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::Index;

/// Bump whenever the schema or the meaning of a field changes: an index written with a
/// different version is discarded and rebuilt.
pub(crate) const SCHEMA_VERSION: u32 = 7;

/// Analyzer for all full-text fields: split on non-alphanumerics, drop >64-byte tokens,
/// lowercase, fold diacritics to ASCII. E-mail addresses therefore index as their parts
/// (`alice`, `example`, `com`); the whole address goes into the raw `*_addr` fields.
pub(crate) const ANALYZER: &str = "stv";

pub(crate) fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(64))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

#[derive(Clone, Copy)]
pub(crate) struct Fields {
    pub source: Field,
    /// `"{source}:{locator json}"`, unique per document; FAST so a source's keys load without the doc store.
    pub key: Field,
    /// Raw `FolderId`.
    pub folder: Field,
    /// Lowercased folder display name and `/`-joined path (multi-valued), for `in:`.
    pub folder_name: Field,
    pub subject: Field,
    /// Normalised subject (reply prefixes stripped, lowercased), FAST for sorting.
    pub subject_sort: Field,
    pub from: Field,
    /// Lowercased sender address; FAST for the sender facet.
    pub from_addr: Field,
    /// Lowercased sender name-or-address, FAST for sorting.
    pub from_sort: Field,
    pub to: Field,
    pub to_addr: Field,
    pub cc: Field,
    pub cc_addr: Field,
    pub bcc: Field,
    pub bcc_addr: Field,
    pub body: Field,
    /// First ~2 KB of body text, stored only: snippet source.
    pub preview: Field,
    pub attachment_names: Field,
    pub attachment_text: Field,
    /// Lowercased labels / categories.
    pub labels: Field,
    /// Flag words: `read`, `unread`, `flagged`, `draft`, `deleted`, `encrypted`, `signed`, and
    /// the item kind: `appointment` (also meeting messages), `meeting`, `contact`, `task`.
    pub is: Field,
    pub message_id: Field,
    pub in_reply_to: Field,
    pub references: Field,
    /// Searchable text of calendar/contact/task details (location, recurrence, company,
    /// phones, postal addresses, …): bare words match it, `body:` does not.
    pub details: Field,
    pub conversation: Field,
    /// Message date, FAST: date sorting and conversation order.
    pub date: Field,
    /// `Header::start_ms` (appointment start / task due date, else the message date), FAST:
    /// `before:`/`after:` and the start sort.
    pub start: Field,
    pub size: Field,
    pub has_attachment: Field,
    /// UTC year of `start`, FAST only: the year facet.
    pub year: Field,
    /// serde_json of the `Header`, stored: hits render without reopening the source.
    pub header: Field,
    /// 1 when the message could not be read and the document holds only its listing
    /// header; such documents are retried on the next sync.
    pub partial: Field,
}

pub(crate) fn text(stored: bool) -> TextOptions {
    let opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_tokenizer(ANALYZER).set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    if stored {
        opts.set_stored()
    } else {
        opts
    }
}

pub(crate) fn build() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let num = NumericOptions::default().set_indexed().set_fast();
    let fast_only = NumericOptions::default().set_fast();
    let fast_str = TextOptions::default().set_fast("raw");
    let f = Fields {
        source: b.add_u64_field("source", num.clone().set_stored()),
        key: b.add_text_field("key", STRING | FAST),
        folder: b.add_text_field("folder", STRING),
        folder_name: b.add_text_field("folder_name", STRING),
        subject: b.add_text_field("subject", text(false)),
        subject_sort: b.add_text_field("subject_sort", fast_str.clone()),
        from: b.add_text_field("from", text(false)),
        from_addr: b.add_text_field("from_addr", STRING | FAST),
        from_sort: b.add_text_field("from_sort", fast_str),
        to: b.add_text_field("to", text(false)),
        to_addr: b.add_text_field("to_addr", STRING),
        cc: b.add_text_field("cc", text(false)),
        cc_addr: b.add_text_field("cc_addr", STRING),
        bcc: b.add_text_field("bcc", text(false)),
        bcc_addr: b.add_text_field("bcc_addr", STRING),
        body: b.add_text_field("body", text(false)),
        preview: b.add_text_field("preview", STORED),
        attachment_names: b.add_text_field("attachment_names", text(false)),
        attachment_text: b.add_text_field("attachment_text", text(false)),
        labels: b.add_text_field("labels", STRING),
        is: b.add_text_field("is", STRING),
        message_id: b.add_text_field("message_id", STRING),
        in_reply_to: b.add_text_field("in_reply_to", STRING),
        references: b.add_text_field("references", STRING),
        conversation: b.add_text_field("conversation", STRING),
        details: b.add_text_field("details", text(false)),
        date: b.add_i64_field("date", num.clone()),
        start: b.add_i64_field("start", num.clone()),
        size: b.add_u64_field("size", num),
        has_attachment: b.add_bool_field("has_attachment", INDEXED),
        year: b.add_u64_field("year", fast_only),
        header: b.add_bytes_field("header", STORED),
        partial: b.add_u64_field("partial", INDEXED),
    };
    (b.build(), f)
}

pub(crate) fn register_analyzers(index: &Index) {
    index.tokenizers().register(ANALYZER, analyzer());
}
