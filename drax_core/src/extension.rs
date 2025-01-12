use crate::transport::{Error, Result, TransportProcessorContext};
use std::io::{Cursor, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

macro_rules! read_var_number {
    ($var_type:ty, $bit_limit:literal, $($reader:tt)*) => {
        {
            let mut value = 0;
            let mut bit_offset = 0u32;

            loop {
                if bit_offset == $bit_limit {
                    return Error::cause("Var int too big, could not find end.");
                }

                let byte = {
                    $($reader)*
                };
                value |= <$var_type>::from(byte & 0b01111111)
                    .overflowing_shl(bit_offset)
                    .0;

                bit_offset += 7;
                if byte & 0b10000000 == 0 {
                    break;
                }
            }
            Ok(value)
        }
    };
}

macro_rules! declare_variable_number {
    ($var_type:ty, $unsigned_type:ty, $size_fn:ident, $read_async:ident, $read_sync:ident, $write_async:ident, $write_sync:ident, $bit_limit:literal, $and_check:literal) => {
        pub async fn $read_async<R: AsyncRead + Unpin>(
            _processor_context: &mut TransportProcessorContext,
            read: &mut R,
        ) -> Result<$var_type> {
            read_var_number!($var_type, $bit_limit, read.read_u8().await?)
        }

        pub fn $read_sync<R: Read>(
            _processor_context: &mut TransportProcessorContext,
            read: &mut R,
        ) -> Result<$var_type> {
            read_var_number!($var_type, $bit_limit, {
                let mut byte = [0; 1];
                let read_amount = read.read(&mut byte)?;
                if read_amount == 0 {
                    return crate::transport::Error::cause("Invalid read, no byte ready.");
                }
                byte[0]
            })
        }

        pub fn $size_fn(
            var: $var_type,
            _processor_context: &mut TransportProcessorContext,
        ) -> Result<usize> {
            let mut temp: $unsigned_type = var as $unsigned_type;
            let mut size = 0;
            loop {
                if temp & $and_check == 0 {
                    return Ok(size + 1);
                }
                size += 1;
                temp = temp.overflowing_shr(7).0;
            }
        }

        pub async fn $write_async<W: AsyncWrite + Unpin>(
            var: $var_type,
            _processor_context: &mut TransportProcessorContext,
            writer: &mut W,
        ) -> Result<()> {
            let mut temp: $unsigned_type = var as $unsigned_type;
            loop {
                if temp & $and_check == 0 {
                    writer.write_u8(temp as u8).await?;
                    return Ok(());
                }
                writer.write_u8((temp & 0x7F | 0x80) as u8).await?;
                temp = temp.overflowing_shr(7).0;
            }
        }

        pub fn $write_sync<W: Write>(
            var: $var_type,
            _processor_context: &mut TransportProcessorContext,
            writer: &mut W,
        ) -> Result<()> {
            let mut temp: $unsigned_type = var as $unsigned_type;
            loop {
                if temp & $and_check == 0 {
                    writer.write_all(&[temp as u8])?;
                    return Ok(());
                }
                writer.write_all(&[(temp & 0x7F | 0x80) as u8])?;
                temp = temp.overflowing_shr(7).0;
            }
        }
    };
}

declare_variable_number!(
    i32,
    u32,
    size_var_int,
    read_var_int,
    read_var_int_sync,
    write_var_int,
    write_var_int_sync,
    35,
    0xFFFFFF80
);

declare_variable_number!(
    i64,
    u64,
    size_var_long,
    read_var_long,
    read_var_long_sync,
    write_var_long,
    write_var_long_sync,
    70,
    0xFFFFFFFFFFFFFF80
);

pub fn write_string_checked<W: Write>(
    bytes: &[u8],
    context: &mut TransportProcessorContext,
    writer: &mut W,
) -> Result<()> {
    write_var_int_sync(bytes.len() as i32, context, writer)?;
    writer.write_all(bytes)?;
    Ok(())
}

pub fn write_string<W: Write>(
    max_length: usize,
    string: &String,
    context: &mut TransportProcessorContext,
    writer: &mut W,
) -> Result<()> {
    let bytes = string.as_bytes();
    let length = bytes.len();
    if length > max_length * 3 {
        return Error::cause(format!(
            "Attempted to write string of length {} when max is {}.",
            length,
            max_length * 4
        ));
    }
    write_string_checked(bytes, context, writer)
}

pub fn read_string_checked<R: Read>(
    length: usize,
    _context: &mut TransportProcessorContext,
    reader: &mut R,
) -> Result<String> {
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes)?;
    let internal = String::from_utf8(bytes)?;
    Ok(internal)
}

pub fn read_string<R: Read>(
    max_length: usize,
    context: &mut TransportProcessorContext,
    reader: &mut R,
) -> Result<String> {
    let length = read_var_int_sync(context, reader)?;
    if (length as usize) > max_length * 3 {
        return Error::cause(format!(
            "Attempted to read string of length {} when max is {}.",
            length,
            max_length * 4
        ));
    }
    if length < 0 {
        return Error::cause(format!(
            "Cannot read a string of less than 0 length. Given {}.",
            length
        ));
    }
    read_string_checked(length as usize, context, reader)
}

pub fn size_string(value: &String, context: &mut TransportProcessorContext) -> Result<usize> {
    let string_len = value.len();
    Ok(size_var_int(string_len as i32, context)? + string_len)
}

pub fn write_json<T, W: Write>(
    max_length: usize,
    value: &T,
    context: &mut TransportProcessorContext,
    writer: &mut W,
) -> Result<()>
where
    T: serde::ser::Serialize,
{
    let value_to_string = serde_json::to_string(value)?;
    write_string(max_length, &value_to_string, context, writer)
}

pub fn size_json<T>(value: &T, context: &mut TransportProcessorContext) -> Result<usize>
where
    T: serde::ser::Serialize,
{
    let value_to_string = serde_json::to_string(value)?;
    size_string(&value_to_string, context)
}

pub fn read_json<T, R: Read>(
    max_length: usize,
    context: &mut TransportProcessorContext,
    reader: &mut R,
) -> Result<T>
where
    T: for<'de> serde::de::Deserialize<'de>,
{
    let json_string = read_string::<R>(max_length, context, reader)?;
    Ok(serde_json::from_slice(json_string.as_bytes())?)
}

impl crate::transport::DraxTransport for uuid::Uuid {
    fn write_to_transport(
        &self,
        context: &mut TransportProcessorContext,
        writer: &mut Cursor<Vec<u8>>,
    ) -> Result<()> {
        let (most_significant, least_significant) = self.as_u64_pair();
        u64::write_to_transport(&most_significant, context, writer)?;
        u64::write_to_transport(&least_significant, context, writer)
    }

    fn read_from_transport<R: Read>(
        context: &mut TransportProcessorContext,
        read: &mut R,
    ) -> Result<Self>
    where
        Self: Sized,
    {
        let (most_significant, least_significant) = (
            u64::read_from_transport(context, read)?,
            u64::read_from_transport(context, read)?,
        );
        Ok(uuid::Uuid::from_u64_pair(
            most_significant,
            least_significant,
        ))
    }

    fn precondition_size(&self, _: &mut TransportProcessorContext) -> Result<usize> {
        Ok(16)
    }
}
