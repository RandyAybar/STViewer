//! Property id constants (`PidTag*`, high 16 bits of a tag) and a name catalogue
//! for the property inspector. Names follow [MS-OXPROPS].

use std::sync::LazyLock;

use crate::{names, PropKey};

macro_rules! tags {
    ($($konst:ident = $id:literal => $name:literal),* $(,)?) => {
        $(#[doc = concat!("`PidTag", $name, "`")] pub const $konst: u16 = $id;)*
        /// `(id, name)`, unsorted; see [`name`].
        static TAG_TABLE: &[(u16, &str)] = &[$(($id, concat!("PidTag", $name))),*];
    };
}

tags! {
    ALTERNATE_RECIPIENT_ALLOWED = 0x0002 => "AlternateRecipientAllowed",
    IMPORTANCE = 0x0017 => "Importance",
    MESSAGE_CLASS = 0x001A => "MessageClass",
    ORIGINATOR_DELIVERY_REPORT_REQUESTED = 0x0023 => "OriginatorDeliveryReportRequested",
    PRIORITY = 0x0026 => "Priority",
    READ_RECEIPT_REQUESTED = 0x0029 => "ReadReceiptRequested",
    RECIPIENT_REASSIGNMENT_PROHIBITED = 0x002B => "RecipientReassignmentProhibited",
    ORIGINAL_SENSITIVITY = 0x002E => "OriginalSensitivity",
    SENSITIVITY = 0x0036 => "Sensitivity",
    SUBJECT = 0x0037 => "Subject",
    CLIENT_SUBMIT_TIME = 0x0039 => "ClientSubmitTime",
    SENT_REPRESENTING_SEARCH_KEY = 0x003B => "SentRepresentingSearchKey",
    SUBJECT_PREFIX = 0x003D => "SubjectPrefix",
    RECEIVED_BY_ENTRY_ID = 0x003F => "ReceivedByEntryId",
    RECEIVED_BY_NAME = 0x0040 => "ReceivedByName",
    SENT_REPRESENTING_ENTRY_ID = 0x0041 => "SentRepresentingEntryId",
    SENT_REPRESENTING_NAME = 0x0042 => "SentRepresentingName",
    RECEIVED_REPRESENTING_ENTRY_ID = 0x0043 => "ReceivedRepresentingEntryId",
    RECEIVED_REPRESENTING_NAME = 0x0044 => "ReceivedRepresentingName",
    MESSAGE_SUBMISSION_ID = 0x0047 => "MessageSubmissionId",
    ORIGINAL_SUBJECT = 0x0049 => "OriginalSubject",
    REPLY_RECIPIENT_ENTRIES = 0x004F => "ReplyRecipientEntries",
    REPLY_RECIPIENT_NAMES = 0x0050 => "ReplyRecipientNames",
    RECEIVED_BY_SEARCH_KEY = 0x0051 => "ReceivedBySearchKey",
    RECEIVED_REPRESENTING_SEARCH_KEY = 0x0052 => "ReceivedRepresentingSearchKey",
    MESSAGE_TO_ME = 0x0057 => "MessageToMe",
    MESSAGE_CC_ME = 0x0058 => "MessageCcMe",
    ORIGINAL_SENDER_NAME = 0x005A => "OriginalSenderName",
    ORIGINAL_SENDER_ENTRY_ID = 0x005B => "OriginalSenderEntryId",
    ORIGINAL_SENDER_SEARCH_KEY = 0x005C => "OriginalSenderSearchKey",
    ORIGINAL_SENT_REPRESENTING_NAME = 0x005D => "OriginalSentRepresentingName",
    START_DATE = 0x0060 => "StartDate",
    END_DATE = 0x0061 => "EndDate",
    OWNER_APPOINTMENT_ID = 0x0062 => "OwnerAppointmentId",
    RESPONSE_REQUESTED = 0x0063 => "ResponseRequested",
    SENT_REPRESENTING_ADDRESS_TYPE = 0x0064 => "SentRepresentingAddressType",
    SENT_REPRESENTING_EMAIL_ADDRESS = 0x0065 => "SentRepresentingEmailAddress",
    ORIGINAL_SENDER_ADDRESS_TYPE = 0x0066 => "OriginalSenderAddressType",
    ORIGINAL_SENDER_EMAIL_ADDRESS = 0x0067 => "OriginalSenderEmailAddress",
    CONVERSATION_TOPIC = 0x0070 => "ConversationTopic",
    CONVERSATION_INDEX = 0x0071 => "ConversationIndex",
    RECEIVED_BY_ADDRESS_TYPE = 0x0075 => "ReceivedByAddressType",
    RECEIVED_BY_EMAIL_ADDRESS = 0x0076 => "ReceivedByEmailAddress",
    RECEIVED_REPRESENTING_ADDRESS_TYPE = 0x0077 => "ReceivedRepresentingAddressType",
    RECEIVED_REPRESENTING_EMAIL_ADDRESS = 0x0078 => "ReceivedRepresentingEmailAddress",
    TRANSPORT_MESSAGE_HEADERS = 0x007D => "TransportMessageHeaders",
    TNEF_CORRELATION_KEY = 0x007F => "TnefCorrelationKey",
    REPORT_DISPOSITION = 0x0080 => "ReportDisposition",
    NON_RECEIPT_NOTIFICATION_REQUESTED = 0x0C06 => "NonReceiptNotificationRequested",
    ORIGINATOR_NON_DELIVERY_REPORT_REQUESTED = 0x0C08 => "OriginatorNonDeliveryReportRequested",
    RECIPIENT_TYPE = 0x0C15 => "RecipientType",
    REPLY_REQUESTED = 0x0C17 => "ReplyRequested",
    SENDER_ENTRY_ID = 0x0C19 => "SenderEntryId",
    SENDER_NAME = 0x0C1A => "SenderName",
    SENDER_SEARCH_KEY = 0x0C1D => "SenderSearchKey",
    SENDER_ADDRESS_TYPE = 0x0C1E => "SenderAddressType",
    SENDER_EMAIL_ADDRESS = 0x0C1F => "SenderEmailAddress",
    DELETE_AFTER_SUBMIT = 0x0E01 => "DeleteAfterSubmit",
    DISPLAY_BCC = 0x0E02 => "DisplayBcc",
    DISPLAY_CC = 0x0E03 => "DisplayCc",
    DISPLAY_TO = 0x0E04 => "DisplayTo",
    PARENT_DISPLAY = 0x0E05 => "ParentDisplay",
    MESSAGE_DELIVERY_TIME = 0x0E06 => "MessageDeliveryTime",
    MESSAGE_FLAGS = 0x0E07 => "MessageFlags",
    MESSAGE_SIZE = 0x0E08 => "MessageSize",
    PARENT_ENTRY_ID = 0x0E09 => "ParentEntryId",
    SENT_MAIL_ENTRY_ID = 0x0E0A => "SentMailEntryId",
    RESPONSIBILITY = 0x0E0F => "Responsibility",
    MESSAGE_RECIPIENTS = 0x0E12 => "MessageRecipients",
    MESSAGE_ATTACHMENTS = 0x0E13 => "MessageAttachments",
    MESSAGE_STATUS = 0x0E17 => "MessageStatus",
    HAS_ATTACHMENTS = 0x0E1B => "HasAttachments",
    NORMALIZED_SUBJECT = 0x0E1D => "NormalizedSubject",
    RTF_IN_SYNC = 0x0E1F => "RtfInSync",
    ATTACH_SIZE = 0x0E20 => "AttachSize",
    ATTACH_NUMBER = 0x0E21 => "AttachNumber",
    INTERNET_ARTICLE_NUMBER = 0x0E23 => "InternetArticleNumber",
    PRIMARY_SEND_ACCOUNT = 0x0E28 => "PrimarySendAccount",
    NEXT_SEND_ACCT = 0x0E29 => "NextSendAcct",
    TO_DO_ITEM_FLAGS = 0x0E2B => "ToDoItemFlags",
    SWAPPED_TO_DO_STORE = 0x0E2C => "SwappedToDoStore",
    SWAPPED_TO_DO_DATA = 0x0E2D => "SwappedToDoData",
    REPL_ITEMID = 0x0E30 => "ReplItemid",
    REPL_CHANGENUM = 0x0E33 => "ReplChangenum",
    REPL_VERSION_HISTORY = 0x0E34 => "ReplVersionHistory",
    REPL_FLAGS = 0x0E38 => "ReplFlags",
    REPL_COPIED_FROM_VERSIONHISTORY = 0x0E3C => "ReplCopiedfromVersionhistory",
    REPL_COPIED_FROM_ITEMID = 0x0E3D => "ReplCopiedfromItemid",
    CREATOR_SID = 0x0E58 => "CreatorSid",
    LAST_MODIFIER_SID = 0x0E59 => "LastModifierSid",
    TRUST_SENDER = 0x0E79 => "TrustSender",
    EXTENDED_RULE_MESSAGE_ACTIONS = 0x0E99 => "ExtendedRuleMessageActions",
    ACCESS = 0x0FF4 => "Access",
    ROW_TYPE = 0x0FF5 => "RowType",
    INSTANCE_KEY = 0x0FF6 => "InstanceKey",
    ACCESS_LEVEL = 0x0FF7 => "AccessLevel",
    MAPPING_SIGNATURE = 0x0FF8 => "MappingSignature",
    RECORD_KEY = 0x0FF9 => "RecordKey",
    STORE_RECORD_KEY = 0x0FFA => "StoreRecordKey",
    STORE_ENTRY_ID = 0x0FFB => "StoreEntryId",
    OBJECT_TYPE = 0x0FFE => "ObjectType",
    ENTRY_ID = 0x0FFF => "EntryId",
    BODY = 0x1000 => "Body",
    REPORT_TEXT = 0x1001 => "ReportText",
    RTF_SYNC_BODY_CRC = 0x1006 => "RtfSyncBodyCrc",
    RTF_SYNC_BODY_COUNT = 0x1007 => "RtfSyncBodyCount",
    RTF_SYNC_BODY_TAG = 0x1008 => "RtfSyncBodyTag",
    RTF_COMPRESSED = 0x1009 => "RtfCompressed",
    RTF_SYNC_PREFIX_COUNT = 0x1010 => "RtfSyncPrefixCount",
    RTF_SYNC_TRAILING_COUNT = 0x1011 => "RtfSyncTrailingCount",
    HTML = 0x1013 => "Html",
    BODY_CONTENT_LOCATION = 0x1014 => "BodyContentLocation",
    BODY_CONTENT_ID = 0x1015 => "BodyContentId",
    NATIVE_BODY = 0x1016 => "NativeBody",
    INTERNET_MESSAGE_ID = 0x1035 => "InternetMessageId",
    INTERNET_REFERENCES = 0x1039 => "InternetReferences",
    IN_REPLY_TO_ID = 0x1042 => "InReplyToId",
    LIST_HELP = 0x1043 => "ListHelp",
    LIST_SUBSCRIBE = 0x1044 => "ListSubscribe",
    LIST_UNSUBSCRIBE = 0x1045 => "ListUnsubscribe",
    ORIGINAL_MESSAGE_ID = 0x1046 => "OriginalMessageId",
    ICON_INDEX = 0x1080 => "IconIndex",
    LAST_VERB_EXECUTED = 0x1081 => "LastVerbExecuted",
    LAST_VERB_EXECUTION_TIME = 0x1082 => "LastVerbExecutionTime",
    FLAG_STATUS = 0x1090 => "FlagStatus",
    FLAG_COMPLETE_TIME = 0x1091 => "FlagCompleteTime",
    FOLLOWUP_ICON = 0x1095 => "FollowupIcon",
    BLOCK_STATUS = 0x1096 => "BlockStatus",
    ITEM_TEMPORARY_FLAGS = 0x1097 => "ItemTemporaryFlags",
    ICALENDAR_START_TIME = 0x10C3 => "ICalendarStartTime",
    ICALENDAR_END_TIME = 0x10C4 => "ICalendarEndTime",
    CDO_RECURRENCEID = 0x10C5 => "CdoRecurrenceid",
    ICALENDAR_REMINDER_NEXT_TIME = 0x10CA => "ICalendarReminderNextTime",
    ATTRIBUTE_HIDDEN = 0x10F4 => "AttributeHidden",
    ATTRIBUTE_READ_ONLY = 0x10F6 => "AttributeReadOnly",
    DISPLAY_NAME = 0x3001 => "DisplayName",
    ADDRESS_TYPE = 0x3002 => "AddressType",
    EMAIL_ADDRESS = 0x3003 => "EmailAddress",
    COMMENT = 0x3004 => "Comment",
    CREATION_TIME = 0x3007 => "CreationTime",
    LAST_MODIFICATION_TIME = 0x3008 => "LastModificationTime",
    SEARCH_KEY = 0x300B => "SearchKey",
    TARGET_ENTRY_ID = 0x3010 => "TargetEntryId",
    CONVERSATION_ID = 0x3013 => "ConversationId",
    CONVERSATION_INDEX_TRACKING = 0x3016 => "ConversationIndexTracking",
    ARCHIVE_TAG = 0x3018 => "ArchiveTag",
    POLICY_TAG = 0x3019 => "PolicyTag",
    RETENTION_PERIOD = 0x301A => "RetentionPeriod",
    START_DATE_ETC = 0x301B => "StartDateEtc",
    RETENTION_DATE = 0x301C => "RetentionDate",
    RETENTION_FLAGS = 0x301D => "RetentionFlags",
    ARCHIVE_PERIOD = 0x301E => "ArchivePeriod",
    ARCHIVE_DATE = 0x301F => "ArchiveDate",
    STORE_SUPPORT_MASK = 0x340D => "StoreSupportMask",
    STORE_STATE = 0x340E => "StoreState",
    VALID_FOLDER_MASK = 0x35DF => "ValidFolderMask",
    IPM_SUBTREE_ENTRY_ID = 0x35E0 => "IpmSubtreeEntryId",
    IPM_OUTBOX_ENTRY_ID = 0x35E2 => "IpmOutboxEntryId",
    IPM_WASTEBASKET_ENTRY_ID = 0x35E3 => "IpmWastebasketEntryId",
    IPM_SENTMAIL_ENTRY_ID = 0x35E4 => "IpmSentMailEntryId",
    VIEWS_ENTRY_ID = 0x35E5 => "ViewsEntryId",
    COMMON_VIEWS_ENTRY_ID = 0x35E6 => "CommonViewsEntryId",
    FINDER_ENTRY_ID = 0x35E7 => "FinderEntryId",
    CONTAINER_FLAGS = 0x3600 => "ContainerFlags",
    FOLDER_TYPE = 0x3601 => "FolderType",
    CONTENT_COUNT = 0x3602 => "ContentCount",
    CONTENT_UNREAD_COUNT = 0x3603 => "ContentUnreadCount",
    SUBFOLDERS = 0x360A => "Subfolders",
    CONTAINER_CLASS = 0x3613 => "ContainerClass",
    ASSOCIATED_CONTENT_COUNT = 0x3617 => "AssociatedContentCount",
    IPM_APPOINTMENT_ENTRY_ID = 0x36D0 => "IpmAppointmentEntryId",
    IPM_CONTACT_ENTRY_ID = 0x36D1 => "IpmContactEntryId",
    IPM_JOURNAL_ENTRY_ID = 0x36D2 => "IpmJournalEntryId",
    IPM_NOTE_ENTRY_ID = 0x36D3 => "IpmNoteEntryId",
    IPM_TASK_ENTRY_ID = 0x36D4 => "IpmTaskEntryId",
    REMINDERS_ONLINE_ENTRY_ID = 0x36D5 => "RemindersOnlineEntryId",
    IPM_DRAFTS_ENTRY_ID = 0x36D7 => "IpmDraftsEntryId",
    ADDITIONAL_REN_ENTRY_IDS = 0x36D8 => "AdditionalRenEntryIds",
    ADDITIONAL_REN_ENTRY_IDS_EX = 0x36D9 => "AdditionalRenEntryIdsEx",
    EXTENDED_FOLDER_FLAGS = 0x36DA => "ExtendedFolderFlags",
    FREE_BUSY_ENTRY_IDS = 0x36E4 => "FreeBusyEntryIds",
    ATTACH_DATA_BINARY = 0x3701 => "AttachDataBinary",
    ATTACH_ENCODING = 0x3702 => "AttachEncoding",
    ATTACH_EXTENSION = 0x3703 => "AttachExtension",
    ATTACH_FILENAME = 0x3704 => "AttachFilename",
    ATTACH_METHOD = 0x3705 => "AttachMethod",
    ATTACH_LONG_FILENAME = 0x3707 => "AttachLongFilename",
    ATTACH_PATHNAME = 0x3708 => "AttachPathname",
    ATTACH_RENDERING = 0x3709 => "AttachRendering",
    ATTACH_TAG = 0x370A => "AttachTag",
    RENDERING_POSITION = 0x370B => "RenderingPosition",
    ATTACH_TRANSPORT_NAME = 0x370C => "AttachTransportName",
    ATTACH_LONG_PATHNAME = 0x370D => "AttachLongPathname",
    ATTACH_MIME_TAG = 0x370E => "AttachMimeTag",
    ATTACH_ADDITIONAL_INFORMATION = 0x370F => "AttachAdditionalInformation",
    ATTACH_MIME_SEQUENCE = 0x3710 => "AttachMimeSequence",
    ATTACH_CONTENT_BASE = 0x3711 => "AttachContentBase",
    ATTACH_CONTENT_ID = 0x3712 => "AttachContentId",
    ATTACH_CONTENT_LOCATION = 0x3713 => "AttachContentLocation",
    ATTACH_FLAGS = 0x3714 => "AttachFlags",
    ATTACH_PAYLOAD_PROVIDER_GUID_STRING = 0x3719 => "AttachPayloadProviderGuidString",
    ATTACH_PAYLOAD_CLASS = 0x371A => "AttachPayloadClass",
    TEXT_ATTACHMENT_CHARSET = 0x371B => "TextAttachmentCharset",
    DISPLAY_TYPE = 0x3900 => "DisplayType",
    DISPLAY_TYPE_EX = 0x3905 => "DisplayTypeEx",
    SMTP_ADDRESS = 0x39FE => "SmtpAddress",
    ADDRESS_BOOK_DISPLAY_NAME_PRINTABLE = 0x39FF => "AddressBookDisplayNamePrintable",
    ACCOUNT = 0x3A00 => "Account",
    GIVEN_NAME = 0x3A06 => "GivenName",
    BUSINESS_TELEPHONE_NUMBER = 0x3A08 => "BusinessTelephoneNumber",
    HOME_TELEPHONE_NUMBER = 0x3A09 => "HomeTelephoneNumber",
    INITIALS = 0x3A0A => "Initials",
    SURNAME = 0x3A11 => "Surname",
    COMPANY_NAME = 0x3A16 => "CompanyName",
    TITLE = 0x3A17 => "Title",
    DEPARTMENT_NAME = 0x3A18 => "DepartmentName",
    OFFICE_LOCATION = 0x3A19 => "OfficeLocation",
    MOBILE_TELEPHONE_NUMBER = 0x3A1C => "MobileTelephoneNumber",
    TRANSMITTABLE_DISPLAY_NAME = 0x3A20 => "TransmittableDisplayName",
    SEND_RICH_INFO = 0x3A40 => "SendRichInfo",
    SEND_INTERNET_ENCODING = 0x3A71 => "SendInternetEncoding",
    INTERNET_CODEPAGE = 0x3FDE => "InternetCodepage",
    HAS_DEFERRED_ACTION_MESSAGES = 0x3FEA => "HasDeferredActionMessages",
    MESSAGE_LOCALE_ID = 0x3FF1 => "MessageLocaleId",
    CREATOR_NAME = 0x3FF8 => "CreatorName",
    CREATOR_ENTRY_ID = 0x3FF9 => "CreatorEntryId",
    LAST_MODIFIER_NAME = 0x3FFA => "LastModifierName",
    LAST_MODIFIER_ENTRY_ID = 0x3FFB => "LastModifierEntryId",
    MESSAGE_CODEPAGE = 0x3FFD => "MessageCodepage",
    SENDER_FLAGS = 0x4019 => "SenderFlags",
    SENT_REPRESENTING_FLAGS = 0x401A => "SentRepresentingFlags",
    INTERNET_MAIL_OVERRIDE_FORMAT = 0x5902 => "InternetMailOverrideFormat",
    MESSAGE_EDITOR_FORMAT = 0x5909 => "MessageEditorFormat",
    SENDER_SMTP_ADDRESS = 0x5D01 => "SenderSmtpAddress",
    SENT_REPRESENTING_SMTP_ADDRESS = 0x5D02 => "SentRepresentingSmtpAddress",
    RECIPIENT_ORDER = 0x5FDF => "RecipientOrder",
    RECIPIENT_DISPLAY_NAME = 0x5FF6 => "RecipientDisplayName",
    RECIPIENT_ENTRY_ID = 0x5FF7 => "RecipientEntryId",
    RECIPIENT_FLAGS = 0x5FFD => "RecipientFlags",
    RECIPIENT_TRACK_STATUS = 0x5FFF => "RecipientTrackStatus",
    SOURCE_KEY = 0x65E0 => "SourceKey",
    PARENT_SOURCE_KEY = 0x65E1 => "ParentSourceKey",
    CHANGE_KEY = 0x65E2 => "ChangeKey",
    PREDECESSOR_CHANGE_LIST = 0x65E3 => "PredecessorChangeList",
    SECURE_SUBMIT_FLAGS = 0x65C6 => "SecureSubmitFlags",
    RIGHTS = 0x6639 => "Rights",
    SORT_LOCALE_ID = 0x6705 => "SortLocaleId",
    LTP_ROW_ID = 0x67F2 => "LtpRowId",
    LTP_ROW_VER = 0x67F3 => "LtpRowVer",
    PST_PASSWORD = 0x67FF => "PstPassword",
    ATTACHMENT_LINK_ID = 0x7FFA => "AttachmentLinkId",
    EXCEPTION_START_TIME = 0x7FFB => "ExceptionStartTime",
    EXCEPTION_END_TIME = 0x7FFC => "ExceptionEndTime",
    ATTACHMENT_FLAGS = 0x7FFD => "AttachmentFlags",
    ATTACHMENT_HIDDEN = 0x7FFE => "AttachmentHidden",
    ATTACHMENT_CONTACT_PHOTO = 0x7FFF => "AttachmentContactPhoto",
}

/// `PidTagAttachDataObject` shares its id with [`ATTACH_DATA_BINARY`] (type PtypObject).
pub const ATTACH_DATA_OBJECT: u16 = ATTACH_DATA_BINARY;
/// `PidTagBodyHtml` shares its id with [`HTML`].
pub const BODY_HTML: u16 = HTML;

// PidTagMessageFlags bits. [MS-OXCMSG] 2.2.1.6
pub const MSGFLAG_READ: i32 = 0x0001;
pub const MSGFLAG_UNMODIFIED: i32 = 0x0002;
pub const MSGFLAG_SUBMIT: i32 = 0x0004;
pub const MSGFLAG_UNSENT: i32 = 0x0008;
pub const MSGFLAG_HASATTACH: i32 = 0x0010;
pub const MSGFLAG_FROMME: i32 = 0x0020;
pub const MSGFLAG_ASSOCIATED: i32 = 0x0040;

// PidTagAttachMethod values. [MS-OXCMSG] 2.2.2.9
pub const ATTACH_BY_VALUE: i32 = 1;
pub const ATTACH_BY_REFERENCE: i32 = 2;
pub const ATTACH_BY_REF_RESOLVE: i32 = 3;
pub const ATTACH_BY_REF_ONLY: i32 = 4;
pub const ATTACH_EMBEDDED_MSG: i32 = 5;
pub const ATTACH_OLE: i32 = 6;
pub const ATTACH_BY_WEB_REFERENCE: i32 = 7;

// PidTagAttachFlags bits. [MS-OXCMSG] 2.2.2.18
pub const ATT_INVISIBLE_IN_HTML: i32 = 0x1;
pub const ATT_INVISIBLE_IN_RTF: i32 = 0x2;
pub const ATT_MHTML_REF: i32 = 0x4;

/// Canonical `PidTag*` / `PidLid*` / `PidName*` name of a property key, if known.
pub fn name(key: &PropKey) -> Option<&'static str> {
    match key {
        PropKey::Id(id) => tag_name(*id),
        PropKey::Named { guid, id } => names::named_name(guid, id),
    }
}

/// Name of a fixed property id (`PidTagSubject` for `0x0037`).
pub fn tag_name(id: u16) -> Option<&'static str> {
    SORTED.binary_search_by_key(&id, |e| e.0).ok().map(|i| SORTED[i].1)
}

static SORTED: LazyLock<Vec<(u16, &'static str)>> = LazyLock::new(|| {
    let mut v = TAG_TABLE.to_vec();
    v.sort_by_key(|e| e.0);
    v
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NamedId;

    #[test]
    fn names_resolve() {
        assert_eq!(tag_name(SUBJECT), Some("PidTagSubject"));
        assert_eq!(tag_name(0x7FFE), Some("PidTagAttachmentHidden"));
        assert_eq!(tag_name(0x0001), None);
        assert_eq!(name(&PropKey::Id(MESSAGE_DELIVERY_TIME)), Some("PidTagMessageDeliveryTime"));
        let kw = PropKey::Named { guid: names::PS_PUBLIC_STRINGS, id: NamedId::Name("Keywords".into()) };
        assert_eq!(name(&kw), Some("PidNameKeywords"));
    }

    #[test]
    fn table_has_no_duplicate_ids() {
        let mut ids: Vec<u16> = TAG_TABLE.iter().map(|e| e.0).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate tag id in TAG_TABLE");
    }
}
