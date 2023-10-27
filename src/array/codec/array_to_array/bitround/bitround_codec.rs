use crate::{
    array::{
        codec::{
            ArrayCodecTraits, ArrayPartialDecoderTraits, ArrayToArrayCodecTraits, CodecError,
            CodecTraits,
        },
        ArrayRepresentation, DataType,
    },
    metadata::Metadata,
};

use super::{bitround_partial_decoder, round_bytes, BitroundCodecConfiguration, IDENTIFIER};

/// A `bitround` codec implementation.
#[derive(Clone, Debug, Default)]
pub struct BitroundCodec {
    keepbits: u32,
}

impl BitroundCodec {
    /// Create a new bitround codec.
    ///
    /// `keepbits` is the number of bits to round to in the floating point mantissa.
    #[must_use]
    pub fn new(keepbits: u32) -> Self {
        Self { keepbits }
    }

    /// Create a new bitround codec from a configuration.
    #[must_use]
    pub fn new_with_configuration(configuration: &BitroundCodecConfiguration) -> Self {
        let BitroundCodecConfiguration::V1(configuration) = configuration;
        Self {
            keepbits: configuration.keepbits,
        }
    }
}

impl CodecTraits for BitroundCodec {
    fn create_metadata(&self) -> Option<Metadata> {
        // FIXME: Output the metadata when the bitround codec is in the zarr specification and supported by multiple implementations.
        // let configuration = BitroundCodecConfigurationV1 {
        //     keepbits: self.keepbits,
        // };
        // Some(Metadata::new_with_serializable_configuration(IDENTIFIER, &configuration).unwrap())
        None
    }

    fn partial_decoder_should_cache_input(&self) -> bool {
        false
    }

    fn partial_decoder_decodes_all(&self) -> bool {
        false
    }
}

impl ArrayCodecTraits for BitroundCodec {
    fn encode(
        &self,
        mut decoded_value: Vec<u8>,
        decoded_representation: &ArrayRepresentation,
    ) -> Result<Vec<u8>, CodecError> {
        round_bytes(
            &mut decoded_value,
            decoded_representation.data_type(),
            self.keepbits,
        )?;
        Ok(decoded_value)
    }

    fn decode(
        &self,
        encoded_value: Vec<u8>,
        _decoded_representation: &ArrayRepresentation,
    ) -> Result<Vec<u8>, CodecError> {
        Ok(encoded_value)
    }
}

impl ArrayToArrayCodecTraits for BitroundCodec {
    fn partial_decoder<'a>(
        &'a self,
        input_handle: Box<dyn ArrayPartialDecoderTraits + 'a>,
    ) -> Box<dyn ArrayPartialDecoderTraits + 'a> {
        Box::new(bitround_partial_decoder::BitroundPartialDecoder::new(
            input_handle,
            self.keepbits,
        ))
    }

    fn compute_encoded_size(
        &self,
        decoded_representation: &ArrayRepresentation,
    ) -> Result<ArrayRepresentation, CodecError> {
        let data_type = decoded_representation.data_type();
        match data_type {
            DataType::Float16 | DataType::BFloat16 | DataType::Float32 | DataType::Float64 => {
                Ok(decoded_representation.clone())
            }
            _ => Err(CodecError::UnsupportedDataType(
                data_type.clone(),
                IDENTIFIER.to_string(),
            )),
        }
    }
}