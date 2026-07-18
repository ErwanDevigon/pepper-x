pub mod decoder;
mod transcriber;
pub mod speaker_filter;

pub use decoder::{
    decode_audio_file, is_supported_audio_extension, DecodeError, DecodedAudio,
    SUPPORTED_AUDIO_EXTENSIONS, TARGET_SAMPLE_RATE,
};
pub use speaker_filter::{filter_other_speakers, SpeakerFilterError, SpeakerFilterResult};
pub use transcriber::{
    load_audio_for_asr, transcribe_wav, StreamingTranscriber, TranscriptionError,
    TranscriptionRequest, TranscriptionResult, BACKEND_NAME,
};
