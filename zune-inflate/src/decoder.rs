#![allow(clippy::never_loop)]

use crate::bitstream::BitStreamReader;
use crate::constants::{
    DEFLATE_BLOCKTYPE_DYNAMIC_HUFFMAN, DEFLATE_BLOCKTYPE_STATIC, DEFLATE_BLOCKTYPE_UNCOMPRESSED,
    DEFLATE_MAX_CODEWORD_LENGTH, DEFLATE_MAX_LITLEN_CODEWORD_LENGTH, DEFLATE_MAX_NUM_SYMS,
    DEFLATE_MAX_OFFSET_CODEWORD_LENGTH, DEFLATE_MAX_PRE_CODEWORD_LEN, DEFLATE_NUM_LITLEN_SYMS,
    DEFLATE_NUM_OFFSET_SYMS, DEFLATE_NUM_PRECODE_SYMS, DEFLATE_PRECODE_LENS_PERMUTATION,
    DELFATE_MAX_LENS_OVERRUN, HUFFDEC_END_OF_BLOCK, HUFFDEC_EXCEPTIONAL, HUFFDEC_LITERAL,
    HUFFDEC_SUITABLE_POINTER, LITLEN_DECODE_BITS, LITLEN_DECODE_RESULTS, LITLEN_ENOUGH,
    LITLEN_TABLE_BITS, OFFSET_DECODE_RESULTS, OFFSET_ENOUGH, OFFSET_TABLEBITS,
    PRECODE_DECODE_RESULTS, PRECODE_ENOUGH, PRECODE_TABLE_BITS
};
use crate::errors::ZlibDecodeErrors;
use crate::utils::{calc_adler_hash, const_copy, copy_rep_matches, make_decode_table_entry};

struct DeflateHeaderTables
{
    litlen_decode_table: [u32; LITLEN_ENOUGH],
    offset_decode_table: [u32; OFFSET_ENOUGH]
}

impl Default for DeflateHeaderTables
{
    fn default() -> Self
    {
        DeflateHeaderTables {
            litlen_decode_table: [0; LITLEN_ENOUGH],
            offset_decode_table: [0; OFFSET_ENOUGH]
        }
    }
}
/// A deflate decoder with wings.
///
/// This one manages it's memory, it pre-allocates a buffer which
/// it tracks number of bytes written and on successfully reaching the
/// end of the block, will return a vector with exactly
/// the number of bytes
pub struct DeflateDecoder<'a>
{
    data:                  &'a [u8],
    position:              usize,
    stream:                BitStreamReader<'a>,
    is_last_block:         bool,
    static_codes_loaded:   bool,
    deflate_header_tables: DeflateHeaderTables
}

impl<'a> DeflateDecoder<'a>
{
    pub fn new(data: &'a [u8]) -> DeflateDecoder<'a>
    {
        // create stream

        DeflateDecoder {
            data,
            position: 0,
            stream: BitStreamReader::new(data),
            is_last_block: false,
            static_codes_loaded: false,
            deflate_header_tables: DeflateHeaderTables::default()
        }
    }
    /// Decode zlib-encoded data returning the uncompressed in a Vec<u8>
    /// or an error of what went wrong.
    pub fn decode_zlib(&mut self) -> Result<Vec<u8>, ZlibDecodeErrors>
    {
        if self.data.len()
            < 2 /* zlib header */
            + 4
        /* Deflate */
        {
            return Err(ZlibDecodeErrors::InsufficientData);
        }

        // Zlib flags
        // See https://www.ietf.org/rfc/rfc1950.txt for
        // the RFC
        let cmf = self.data[0];
        let flg = self.data[1];

        let cm = cmf & 0xF;
        let cinfo = cmf >> 4;

        // let fcheck = flg & 0xF;
        // let fdict = (flg >> 4) & 1;
        // let flevel = flg >> 5;

        // confirm we have the right deflate methods
        if cm != 8
        {
            if cm == 15
            {
                return Err(ZlibDecodeErrors::Generic(
                    "CM of 15 is preserved by the standard,currently don't know how to handle it"
                ));
            }
            return Err(ZlibDecodeErrors::GenericStr(format!(
                "Unknown zlib compression method {cm}"
            )));
        }
        if cinfo > 7
        {
            return Err(ZlibDecodeErrors::GenericStr(format!(
                "Unknown cinfo `{cinfo}` greater than 7, not allowed"
            )));
        }
        let flag_checks = (u16::from(cmf) * 256) + u16::from(flg);

        if flag_checks % 31 != 0
        {
            return Err(ZlibDecodeErrors::Generic("FCHECK integrity not preserved"));
        }

        self.position = 2;

        self.decode_deflate()
    }
    /// Decode a deflate stream returning the data as Vec<u8> or an error
    /// indicating what went wrong.
    pub fn decode_deflate(&mut self) -> Result<Vec<u8>, ZlibDecodeErrors>
    {
        self.start_deflate_block()
    }
    /// Main inner loop for decompressing
    #[allow(unused_assignments)]
    fn start_deflate_block(&mut self) -> Result<Vec<u8>, ZlibDecodeErrors>
    {
        const FASTCOPY_BITS: usize = 16;
        // start deflate decode
        // re-read the stream so that we can remove code read by zlib
        self.stream = BitStreamReader::new(&self.data[self.position..]);

        self.stream.refill();

        // Output space for our decoded bytes.
        let mut out_block = vec![0; 37000];
        // bits used

        let mut src_offset = 0;
        let mut dest_offset = 0;

        loop
        {
            if !self.stream.has(3)
            {
                return Err(ZlibDecodeErrors::InsufficientData);
            }
            self.is_last_block = self.stream.get_bits(1) == 1;
            let block_type = self.stream.get_bits(2);

            let overread_count = 0;

            if block_type == DEFLATE_BLOCKTYPE_UNCOMPRESSED
            {
                /*
                 * Uncompressed block: copy 'len' bytes literally from the input
                 * buffer to the output buffer.
                 */
                /*
                 * The RFC says that
                 * skip any remaining bits in current partially
                 *       processed byte
                 *     read LEN and NLEN (see next section)
                 *     copy LEN bytes of data to output
                 */

                if overread_count > self.stream.get_bits_left() >> 3
                {
                    return Err(ZlibDecodeErrors::Generic("Over-read stream"));
                }
                let partial_bits = self.stream.get_bits_left() & 7;
                // advance if we have extra bits
                if !self.stream.has(32 + partial_bits)
                {
                    return Err(ZlibDecodeErrors::InsufficientData);
                }
                self.stream.drop_bits(partial_bits);
                let len = self.stream.get_bits(16) as usize;
                let nlen = self.stream.get_bits(16) as usize;

                // copy to deflate
                if len != !nlen
                {
                    return Err(ZlibDecodeErrors::Generic("Len and nlen do not match"));
                }

                let start = self.stream.get_position() + self.position;

                let curr_len = out_block.len();

                out_block.resize(len + curr_len, 0);
                out_block[curr_len..curr_len + len].copy_from_slice(&self.data[start..start + len]);

                if self.is_last_block
                {
                    break;
                }

                self.stream.reset();

                continue;
            }

            // build decode tables for static and dynamic tables
            self.build_decode_table(block_type)?;

            // Tables are mutated into the struct, so at this point we know the tables
            // are loaded, take a reference to them
            let litlen_decode_table = &self.deflate_header_tables.litlen_decode_table;
            let offset_decode_table = &self.deflate_header_tables.offset_decode_table;

            /*
             * This is the "fast loop" for decoding literals and matches.  It does
             * bounds checks on in_next and out_next in the loop conditions so that
             * additional bounds checks aren't needed inside the loop body.
             *
             * To reduce latency, the bit-buffer is refilled and the next litlen
             * decode table entry is preloaded before each loop iteration.
             */
            let (mut literal, mut length, mut offset, mut entry) = (0, 0, 0, 0);

            let mut saved_bitbuf;

            'decode: loop
            {
                let close_src = 3 * FASTCOPY_BITS < self.stream.remaining_bytes();

                if close_src
                {
                    self.stream.refill();

                    let lit_mask = self.stream.peek_bits::<LITLEN_DECODE_BITS>();

                    entry = litlen_decode_table[lit_mask];

                    'sequence: loop
                    {
                        // At this point entry contains the next value of the litlen
                        // This will always be the case so meaning all our exit paths need
                        // to load in the next entry.

                        // recheck after every sequence
                        // when we hit continue, we need to recheck this
                        // as we are trying to emulate a do while
                        let new_check = 2 * FASTCOPY_BITS > self.stream.remaining_bytes();

                        if new_check
                        {
                            break 'sequence;
                        }

                        self.stream.refill();
                        /*
                         * Consume the bits for the litlen decode table entry.  Save the
                         * original bit-buf for later, in case the extra match length
                         * bits need to be extracted from it.
                         */
                        saved_bitbuf = self.stream.buffer;

                        self.stream.drop_bits((entry & 0xFF) as u8);

                        /*
                         * Begin by checking for a "fast" literal, i.e. a literal that
                         * doesn't need a subtable.
                         */
                        if (entry & HUFFDEC_LITERAL) != 0
                        {
                            /*
                             * On 64-bit platforms, we decode up to 2 extra fast
                             * literals in addition to the primary item, as this
                             * increases performance and still leaves enough bits
                             * remaining for what follows.  We could actually do 3,
                             * assuming LITLEN_TABLEBITS=11, but that actually
                             * decreases performance slightly (perhaps by messing
                             * with the branch prediction of the conditional refill
                             * that happens later while decoding the match offset).
                             */

                            let new_pos = self.stream.peek_bits::<LITLEN_DECODE_BITS>();

                            literal = entry >> 16;
                            entry = litlen_decode_table[new_pos];
                            saved_bitbuf = self.stream.buffer;

                            self.stream.drop_bits(entry as u8);

                            resize_and_push(&mut out_block, dest_offset, literal as u8);
                            dest_offset += 1;

                            if (entry & HUFFDEC_LITERAL) != 0
                            {
                                /*
                                 * Another fast literal, but this one is in lieu of the
                                 * primary item, so it doesn't count as one of the extras.
                                 */
                                let new_pos = self.stream.peek_bits::<LITLEN_DECODE_BITS>();

                                // load in the next entry.
                                literal = entry >> 16;
                                entry = litlen_decode_table[new_pos];

                                resize_and_push(&mut out_block, dest_offset, literal as u8);
                                dest_offset += 1;

                                continue;
                            }
                        }
                        /*
                         * It's not a literal entry, so it can be a length entry, a
                         * subtable pointer entry, or an end-of-block entry.  Detect the
                         * two unlikely cases by testing the HUFFDEC_EXCEPTIONAL flag.
                         */
                        if (entry & HUFFDEC_EXCEPTIONAL) != 0
                        {
                            // Subtable pointer or end of block entry
                            if (entry & HUFFDEC_END_OF_BLOCK) != 0
                            {
                                // block done
                                break 'decode;
                            }
                            /*
                             * A subtable is required.  Load and consume the
                             * subtable entry.  The subtable entry can be of any
                             * type: literal, length, or end-of-block.
                             */
                            let entry_position = ((entry >> 8) & 0x3F) as usize;
                            let mut pos = (entry >> 16) as usize;

                            saved_bitbuf = self.stream.buffer;

                            pos += self.stream.peek_var_bits(entry_position);
                            entry = litlen_decode_table[pos];

                            self.stream.drop_bits(entry as u8);

                            if (entry & HUFFDEC_LITERAL) != 0
                            {
                                // decode a literal that required a sub table
                                let new_pos = self.stream.peek_bits::<LITLEN_DECODE_BITS>();

                                literal = entry >> 16;
                                entry = litlen_decode_table[new_pos];

                                resize_and_push(&mut out_block, dest_offset, literal as u8);
                                dest_offset += 1;

                                continue;
                            }

                            if (entry & HUFFDEC_END_OF_BLOCK) != 0
                            {
                                break 'decode;
                            }
                        }

                        //  At this point,we dropped at most 22 bits(LITLEN_DECODE is 11 and we
                        // can do it twice), we now just have 34 bits min remaining.

                        /*
                         * Decode the match length: the length base value associated
                         * with the litlen symbol (which we extract from the decode
                         * table entry), plus the extra length bits.  We don't need to
                         * consume the extra length bits here, as they were included in
                         * the bits consumed by the entry earlier.  We also don't need
                         * to check for too-long matches here, as this is inside the
                         * fast loop where it's already been verified that the output
                         * buffer has enough space remaining to copy a max-length match.
                         */
                        length = (entry >> 16) as usize;

                        let mask = (1 << entry as u8) - 1;

                        length += (saved_bitbuf & mask) as usize >> ((entry >> 8) as u8);

                        entry = offset_decode_table[self.stream.peek_bits::<OFFSET_TABLEBITS>()];

                        // offset requires a subtable
                        if (entry & HUFFDEC_EXCEPTIONAL) != 0
                        {
                            self.stream.drop_bits(OFFSET_TABLEBITS as u8);

                            let extra = self.stream.peek_var_bits(((entry >> 8) & 0x3F) as usize);

                            entry = offset_decode_table[((entry >> 16) as usize + extra) & 511];
                        }

                        saved_bitbuf = self.stream.buffer;

                        self.stream.drop_bits((entry & 0xFF) as u8);

                        let mask = (1 << entry as u8) - 1;

                        offset = (entry >> 16) as usize;
                        offset += (saved_bitbuf & mask) as usize >> ((entry >> 8) as u8);

                        if offset > dest_offset
                        {
                            return Err(ZlibDecodeErrors::CorruptData);
                        }

                        src_offset = dest_offset - offset;

                        // ensure there is enough space for a fast copy
                        if dest_offset + length + FASTCOPY_BITS > out_block.len()
                        {
                            // and if there is not, resize
                            let new_len = out_block.len() + RESIZE_BY + length;

                            out_block.resize(new_len, 0);
                        }

                        let (dest_src, dest_ptr) = out_block.split_at_mut(dest_offset);

                        entry = litlen_decode_table[self.stream.peek_bits::<LITLEN_DECODE_BITS>()];

                        // Copy some bytes unconditionally
                        // This makes us copy smaller match lengths quicker because we don't need
                        // a loop+ don't send too much pressure to the Memory unit.
                        const_copy::<FASTCOPY_BITS, false>(dest_src, dest_ptr, src_offset, 0);

                        if offset == 1
                        {
                            // RLE match, copy it in groups of 8
                            let rep_num = u64::from(dest_src[src_offset]) * 0x0101010101010101;
                            let rep_byte = rep_num.to_ne_bytes();

                            // number of bytes we can copy per loop
                            const N_BYTES: usize = (u64::BITS / u8::BITS) as usize;

                            let mut bytes_written = 0;

                            loop
                            {
                                // Safety
                                // We resized this to enable sloppy copies
                                // (remember we control our output)
                                const_copy::<N_BYTES, false>(&rep_byte, dest_ptr, 0, bytes_written);
                                bytes_written += N_BYTES;

                                if bytes_written > length
                                {
                                    break;
                                }
                            }
                        }
                        else if src_offset + length + FASTCOPY_BITS > dest_offset
                        {
                            // overlapping copy
                            // do a simple rep match
                            copy_rep_matches(&mut out_block, src_offset, dest_offset, length);
                        }
                        else if length > FASTCOPY_BITS
                        {
                            // fast non-overlapping copy
                            //
                            // We have enough space to write the ML+FAST_COPY bytes ahead
                            // so we know this won't come to shoot us in the foot.
                            //
                            // An optimization is to copy FAST_COPY_BITS per invocation
                            // Currently FASTCOPY_BITS is 16, this fits in nicely as we
                            // it's a single SIMD instruction on a lot of things, i.e x86,Arm and even
                            // wasm.

                            // current position of the match
                            let mut dest_src_offset = src_offset + FASTCOPY_BITS;

                            // current position where the destination offset should be
                            let mut dest_dst_offset = FASTCOPY_BITS;

                            // Number of bytes we are to copy
                            let mut ml_copy = length;
                            // copy in batches of FAST_BITS
                            'match_lengths: loop
                            {
                                // No need to be safe here,
                                // we resized this to allow such things above
                                const_copy::<FASTCOPY_BITS, false>(
                                    dest_src,
                                    dest_ptr,
                                    dest_src_offset,
                                    dest_dst_offset
                                );

                                dest_src_offset += FASTCOPY_BITS;
                                dest_dst_offset += FASTCOPY_BITS;

                                if ml_copy < 2 * FASTCOPY_BITS
                                {
                                    // we copied FAST_BITS above in this loop
                                    // and we copied another one in our unconditional copy
                                    // so if we are less than the above, we know we are done.
                                    break 'match_lengths;
                                }

                                ml_copy = ml_copy.saturating_sub(FASTCOPY_BITS);
                            }
                        }

                        dest_offset += length;

                        if 2 * FASTCOPY_BITS > self.stream.remaining_bytes()
                        {
                            // close to input end, move to the slower one
                            break 'sequence;
                        }
                    }
                }
                // generic loop that does things a bit slower but it's okay since it doesn't
                // deal with a lot of things
                // We can afford to be more careful here, checking that we do
                // not drop non-existent bits etc etc as we do not have the
                // assurances of the fast loop bits above.
                loop
                {
                    self.stream.refill();

                    let literal_mask = self.stream.peek_bits::<LITLEN_DECODE_BITS>();

                    entry = litlen_decode_table[literal_mask];

                    saved_bitbuf = self.stream.buffer;

                    if !self.stream.has((entry & 0xFF) as u8)
                    {
                        return Err(ZlibDecodeErrors::InsufficientData);
                    }

                    self.stream.drop_bits((entry & 0xFF) as u8);

                    if (entry & HUFFDEC_SUITABLE_POINTER) != 0
                    {
                        let extra = self.stream.peek_var_bits(((entry >> 8) & 0x3F) as usize);

                        entry = litlen_decode_table[(entry >> 16) as usize + extra];
                        saved_bitbuf = self.stream.buffer;

                        self.stream.drop_bits((entry & 0xFF) as u8);
                    }
                    length = (entry >> 16) as usize;

                    if (entry & HUFFDEC_LITERAL) != 0
                    {
                        resize_and_push(&mut out_block, dest_offset, length as u8);

                        dest_offset += 1;

                        continue;
                    }

                    if (entry & HUFFDEC_END_OF_BLOCK) != 0
                    {
                        break 'decode;
                    }

                    let mask = (1 << entry as u8) - 1;

                    length += (saved_bitbuf & mask) as usize >> ((entry >> 8) as u8);

                    self.stream.refill();

                    entry = offset_decode_table[self.stream.peek_bits::<OFFSET_TABLEBITS>()];

                    if (entry & HUFFDEC_EXCEPTIONAL) != 0
                    {
                        // offset requires a subtable
                        self.stream.drop_bits(OFFSET_TABLEBITS as u8);

                        let extra = self.stream.peek_var_bits(((entry >> 8) & 0x3F) as usize);

                        entry = offset_decode_table[((entry >> 16) as usize + extra) & 511];
                    }

                    // ensure there is enough space for a fast copy
                    if dest_offset + length + FASTCOPY_BITS > out_block.len()
                    {
                        let new_len = out_block.len() + RESIZE_BY + length;
                        out_block.resize(new_len, 0);
                    }
                    saved_bitbuf = self.stream.buffer;

                    let mask = (1 << (entry & 0xFF) as u8) - 1;

                    offset = (entry >> 16) as usize;
                    offset += (saved_bitbuf & mask) as usize >> ((entry >> 8) as u8);

                    if offset > dest_offset
                    {
                        return Err(ZlibDecodeErrors::CorruptData);
                    }

                    src_offset = dest_offset - offset;

                    if !self.stream.has((entry & 0xFF) as u8)
                    {
                        return Err(ZlibDecodeErrors::InsufficientData);
                    }

                    self.stream.drop_bits(entry as u8);

                    let (dest_src, dest_ptr) = out_block.split_at_mut(dest_offset);

                    if src_offset + length + FASTCOPY_BITS > dest_offset
                    {
                        // overlapping copy
                        // do a simple rep match
                        copy_rep_matches(&mut out_block, src_offset, dest_offset, length);
                    }
                    else
                    {
                        dest_ptr[0..length]
                            .copy_from_slice(&dest_src[src_offset..src_offset + length]);
                    }

                    dest_offset += length;
                }
            }

            if self.is_last_block
            {
                break;
            }
        }
        // revert bitstream back to last read bits
        let out_pos = self.stream.get_position() - usize::from(self.stream.bits_left >> 3);

        // decompression. DONE
        // Truncate data to match the number of actual
        // bytes written.
        out_block.truncate(dest_offset);

        // read adler
        {
            let adler_bits: [u8; 4] = self.data
                [self.position + out_pos..self.position + out_pos + 4]
                .try_into()
                .unwrap();

            let adler32_expected = u32::from_be_bytes(adler_bits);

            let adler32_found = calc_adler_hash(&out_block);

            assert_eq!(adler32_expected, adler32_found);
        }

        Ok(out_block)
    }

    /// Build decode tables for static and dynamic
    /// huffman blocks.
    fn build_decode_table(&mut self, block_type: u64) -> Result<(), ZlibDecodeErrors>
    {
        const COUNT: usize =
            DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS + DELFATE_MAX_LENS_OVERRUN;

        let mut lens = [0_u8; COUNT];
        let mut precode_lens = [0; DEFLATE_NUM_PRECODE_SYMS];
        let mut precode_decode_table = [0_u32; PRECODE_ENOUGH];
        let mut litlen_decode_table = [0_u32; LITLEN_ENOUGH];
        let mut offset_decode_table = [0; OFFSET_ENOUGH];

        let mut num_litlen_syms = 0;
        let mut num_offset_syms = 0;

        if block_type == DEFLATE_BLOCKTYPE_DYNAMIC_HUFFMAN
        {
            const SINGLE_PRECODE: usize = 3;

            self.static_codes_loaded = false;

            // Dynamic Huffman block
            // Read codeword lengths
            if !self.stream.has(5 + 5 + 4)
            {
                return Err(ZlibDecodeErrors::InsufficientData);
            }

            num_litlen_syms = 257 + (self.stream.get_bits(5)) as usize;
            num_offset_syms = 1 + (self.stream.get_bits(5)) as usize;

            let num_explicit_precode_lens = 4 + (self.stream.get_bits(4)) as usize;

            self.stream.refill();

            if !self.stream.has(3)
            {
                return Err(ZlibDecodeErrors::InsufficientData);
            }

            let first_precode = self.stream.get_bits(3) as u8;
            let expected = (SINGLE_PRECODE * num_explicit_precode_lens.saturating_sub(1)) as u8;

            precode_lens[usize::from(DEFLATE_PRECODE_LENS_PERMUTATION[0])] = first_precode;

            self.stream.refill();

            if !self.stream.has(expected)
            {
                return Err(ZlibDecodeErrors::InsufficientData);
            }

            for i in DEFLATE_PRECODE_LENS_PERMUTATION[1..]
                .iter()
                .take(num_explicit_precode_lens - 1)
            {
                let bits = self.stream.get_bits(3) as u8;

                precode_lens[usize::from(*i)] = bits;
            }

            self.build_decode_table_inner(
                &precode_lens,
                &PRECODE_DECODE_RESULTS,
                &mut precode_decode_table,
                PRECODE_TABLE_BITS,
                DEFLATE_NUM_PRECODE_SYMS,
                DEFLATE_MAX_CODEWORD_LENGTH
            )?;

            /* Decode the litlen and offset codeword lengths. */

            let mut i = 0;

            loop
            {
                if i >= num_litlen_syms + num_offset_syms
                {
                    // confirm here since with a continue loop stuff
                    // breaks
                    break;
                }

                let rep_val: u8;
                let rep_count: u64;

                if !self.stream.has(DEFLATE_MAX_PRE_CODEWORD_LEN + 7)
                {
                    self.stream.refill();
                }
                // decode next pre-code symbol
                let entry_pos = self
                    .stream
                    .peek_bits::<{ DEFLATE_MAX_PRE_CODEWORD_LEN as usize }>();

                let entry = precode_decode_table[entry_pos];
                let presym = entry >> 16;

                if !self.stream.has(entry as u8)
                {
                    return Err(ZlibDecodeErrors::InsufficientData);
                }

                self.stream.drop_bits(entry as u8);

                if presym < 16
                {
                    // explicit codeword length
                    lens[i] = presym as u8;
                    i += 1;
                    continue;
                }

                /* Run-length encoded codeword lengths */

                /*
                 * Note: we don't need verify that the repeat count
                 * doesn't overflow the number of elements, since we've
                 * sized the lens array to have enough extra space to
                 * allow for the worst-case overrun (138 zeroes when
                 * only 1 length was remaining).
                 *
                 * In the case of the small repeat counts (presyms 16
                 * and 17), it is fastest to always write the maximum
                 * number of entries.  That gets rid of branches that
                 * would otherwise be required.
                 *
                 * It is not just because of the numerical order that
                 * our checks go in the order 'presym < 16', 'presym ==
                 * 16', and 'presym == 17'.  For typical data this is
                 * ordered from most frequent to least frequent case.
                 */
                if presym == 16
                {
                    if i == 0
                    {
                        return Err(ZlibDecodeErrors::CorruptData);
                    }

                    if !self.stream.has(2)
                    {
                        return Err(ZlibDecodeErrors::InsufficientData);
                    }

                    // repeat previous length three to 6 times
                    rep_val = lens[i - 1];
                    rep_count = 3 + self.stream.get_bits(2);
                    lens[i..i + 6].fill(rep_val);
                    i += rep_count as usize;
                }
                else if presym == 17
                {
                    if !self.stream.has(3)
                    {
                        return Err(ZlibDecodeErrors::InsufficientData);
                    }
                    /* Repeat zero 3 - 10 times. */
                    rep_count = 3 + self.stream.get_bits(3);
                    lens[i..i + 10].fill(0);
                    i += rep_count as usize;
                }
                else
                {
                    if !self.stream.has(7)
                    {
                        return Err(ZlibDecodeErrors::InsufficientData);
                    }
                    // repeat zero 11-138 times.
                    rep_count = 11 + self.stream.get_bits(7);
                    lens[i..i + rep_count as usize].fill(0);
                    i += rep_count as usize;
                }

                if i >= num_litlen_syms + num_offset_syms
                {
                    break;
                }
            }
        }
        else if block_type == DEFLATE_BLOCKTYPE_STATIC
        {
            if self.static_codes_loaded
            {
                return Ok(());
            }

            self.static_codes_loaded = true;

            lens[000..144].fill(8);
            lens[144..256].fill(9);
            lens[256..280].fill(7);
            lens[280..288].fill(8);
            lens[288..].fill(5);

            num_litlen_syms = 288;
            num_offset_syms = 32;
        }
        // build offset decode table
        self.build_decode_table_inner(
            &lens[num_litlen_syms..],
            &OFFSET_DECODE_RESULTS,
            &mut offset_decode_table,
            OFFSET_TABLEBITS,
            num_offset_syms,
            DEFLATE_MAX_OFFSET_CODEWORD_LENGTH
        )?;

        self.build_decode_table_inner(
            &lens,
            &LITLEN_DECODE_RESULTS,
            &mut litlen_decode_table,
            LITLEN_TABLE_BITS,
            num_litlen_syms,
            DEFLATE_MAX_LITLEN_CODEWORD_LENGTH
        )?;

        self.deflate_header_tables.offset_decode_table = offset_decode_table;
        self.deflate_header_tables.litlen_decode_table = litlen_decode_table;

        Ok(())
    }
    /// Build the decode table for the precode
    #[allow(clippy::needless_range_loop)]
    fn build_decode_table_inner(
        &mut self, lens: &[u8], decode_results: &[u32], decode_table: &mut [u32],
        table_bits: usize, num_syms: usize, mut max_codeword_len: usize
    ) -> Result<(), ZlibDecodeErrors>
    {
        const BITS: u32 = usize::BITS - 1;

        let mut len_counts: [u32; DEFLATE_MAX_CODEWORD_LENGTH + 1] =
            [0; DEFLATE_MAX_CODEWORD_LENGTH + 1];
        let mut offsets: [u32; DEFLATE_MAX_CODEWORD_LENGTH + 1] =
            [0; DEFLATE_MAX_CODEWORD_LENGTH + 1];
        let mut sorted_syms: [u16; DEFLATE_MAX_NUM_SYMS] = [0; DEFLATE_MAX_NUM_SYMS];

        let mut i;

        // count how many codewords have each length, including 0.
        for sym in 0..num_syms
        {
            len_counts[usize::from(lens[sym])] += 1;
        }

        /*
         * Determine the actual maximum codeword length that was used, and
         * decrease table_bits to it if allowed.
         */
        while max_codeword_len > 1 && len_counts[max_codeword_len] == 0
        {
            max_codeword_len -= 1;
        }
        /*
         * Sort the symbols primarily by increasing codeword length and
         *	A temporary array of length @num_syms.
         * secondarily by increasing symbol value; or equivalently by their
         * codewords in lexicographic order, since a canonical code is assumed.
         *
         * For efficiency, also compute 'codespace_used' in the same pass over
         * 'len_counts[]' used to build 'offsets[]' for sorting.
         */
        offsets[0] = 0;
        offsets[1] = len_counts[0];

        let mut codespace_used = 0_u32;

        for len in 1..max_codeword_len
        {
            offsets[len + 1] = offsets[len] + len_counts[len];
            codespace_used = (codespace_used << 1) + len_counts[len];
        }
        codespace_used = (codespace_used << 1) + len_counts[max_codeword_len];

        for sym in 0..num_syms
        {
            let pos = usize::from(lens[sym]);
            sorted_syms[offsets[pos] as usize] = sym as u16;
            offsets[pos] += 1;
        }
        i = (offsets[0]) as usize;

        /*
         * Check whether the lengths form a complete code (exactly fills the
         * codespace), an incomplete code (doesn't fill the codespace), or an
         * overfull code (overflows the codespace).  A codeword of length 'n'
         * uses proportion '1/(2^n)' of the codespace.  An overfull code is
         * nonsensical, so is considered invalid.  An incomplete code is
         * considered valid only in two specific cases; see below.
         */

        // Overfull code
        if codespace_used > 1 << max_codeword_len
        {
            return Err(ZlibDecodeErrors::Generic("Overflown code"));
        }
        // incomplete code
        if codespace_used < 1 << max_codeword_len
        {
            let entry = if codespace_used == 0
            {
                /*
                 * An empty code is allowed.  This can happen for the
                 * offset code in DEFLATE, since a dynamic Huffman block
                 * need not contain any matches.
                 */

                /* sym=0, len=1 (arbitrary) */
                make_decode_table_entry(decode_results, 0, 1)
            }
            else
            {
                /*
                 * Allow codes with a single used symbol, with codeword
                 * length 1.  The DEFLATE RFC is unclear regarding this
                 * case.  What zlib's decompressor does is permit this
                 * for the litlen and offset codes and assume the
                 * codeword is '0' rather than '1'.  We do the same
                 * except we allow this for precodes too, since there's
                 * no convincing reason to treat the codes differently.
                 * We also assign both codewords '0' and '1' to the
                 * symbol to avoid having to handle '1' specially.
                 */
                if codespace_used != 1 << max_codeword_len || len_counts[1] != 1
                {
                    return Err(ZlibDecodeErrors::Generic(
                        "Cannot work with empty pre-code table"
                    ));
                }
                make_decode_table_entry(decode_results, usize::from(sorted_syms[i]), 1)
            };
            /*
             * Note: the decode table still must be fully initialized, in
             * case the stream is malformed and contains bits from the part
             * of the codespace the incomplete code doesn't use.
             */
            decode_table.fill(entry);
            return Ok(());
        }

        /*
         * The lengths form a complete code.  Now, enumerate the codewords in
         * lexicographic order and fill the decode table entries for each one.
         *
         * First, process all codewords with len <= table_bits.  Each one gets
         * '2^(table_bits-len)' direct entries in the table.
         *
         * Since DEFLATE uses bit-reversed codewords, these entries aren't
         * consecutive but rather are spaced '2^len' entries apart.  This makes
         * filling them naively somewhat awkward and inefficient, since strided
         * stores are less cache-friendly and preclude the use of word or
         * vector-at-a-time stores to fill multiple entries per instruction.
         *
         * To optimize this, we incrementally double the table size.  When
         * processing codewords with length 'len', the table is treated as
         * having only '2^len' entries, so each codeword uses just one entry.
         * Then, each time 'len' is incremented, the table size is doubled and
         * the first half is copied to the second half.  This significantly
         * improves performance over naively doing strided stores.
         *
         * Note that some entries copied for each table doubling may not have
         * been initialized yet, but it doesn't matter since they're guaranteed
         * to be initialized later (because the Huffman code is complete).
         */
        let mut codeword = 0;
        let mut len = 1;
        let mut count = len_counts[1];

        while count == 0
        {
            len += 1;

            if len >= len_counts.len()
            {
                break;
            }
            count = len_counts[len];
        }

        let mut curr_table_end = 1 << len;

        while len <= table_bits
        {
            // Process all count codewords with length len

            loop
            {
                let entry = make_decode_table_entry(
                    decode_results,
                    usize::from(sorted_syms[i]),
                    len as u32
                );
                i += 1;
                // fill first entry for current codeword
                decode_table[codeword] = entry;

                if codeword == curr_table_end - 1
                {
                    // last codeword (all 1's)
                    for _ in len..table_bits
                    {
                        decode_table.copy_within(0..curr_table_end, curr_table_end);

                        curr_table_end <<= 1;
                    }
                    return Ok(());
                }
                /*
                 * To advance to the lexicographically next codeword in
                 * the canonical code, the codeword must be incremented,
                 * then 0's must be appended to the codeword as needed
                 * to match the next codeword's length.
                 *
                 * Since the codeword is bit-reversed, appending 0's is
                 * a no-op.  However, incrementing it is nontrivial.  To
                 * do so efficiently, use the 'bsr' instruction to find
                 * the last (highest order) 0 bit in the codeword, set
                 * it, and clear any later (higher order) 1 bits.  But
                 * 'bsr' actually finds the highest order 1 bit, so to
                 * use it first flip all bits in the codeword by XOR' ing
                 * it with (1U << len) - 1 == cur_table_end - 1.
                 */

                let adv = BITS - (codeword ^ (curr_table_end - 1)).leading_zeros();
                let bit = 1 << adv;

                codeword &= bit - 1;
                codeword |= bit;
                count -= 1;

                if count == 0
                {
                    break;
                }
            }
            // advance to the next codeword length
            loop
            {
                len += 1;

                if len <= table_bits
                {
                    // dest is decode_table[curr_table_end]
                    // source is decode_table(start of table);
                    // size is curr_table;

                    decode_table.copy_within(0..curr_table_end, curr_table_end);

                    //decode_table.copy_within(range, curr_table_end);
                    curr_table_end <<= 1;
                }
                count = len_counts[len];

                if count != 0
                {
                    break;
                }
            }
        }
        // process codewords with len > table_bits.
        // Require sub-tables
        curr_table_end = 1 << table_bits;

        let mut subtable_prefix = usize::MAX;
        let mut subtable_start = 0;
        let mut subtable_bits;

        loop
        {
            /*
             * Start a new sub-table if the first 'table_bits' bits of the
             * codeword don't match the prefix of the current subtable.
             */
            if codeword & ((1_usize << table_bits) - 1) != subtable_prefix
            {
                subtable_prefix = codeword & ((1 << table_bits) - 1);
                subtable_start = curr_table_end;

                /*
                 * Calculate the subtable length.  If the codeword has
                 * length 'table_bits + n', then the subtable needs
                 * '2^n' entries.  But it may need more; if fewer than
                 * '2^n' codewords of length 'table_bits + n' remain,
                 * then the length will need to be incremented to bring
                 * in longer codewords until the subtable can be
                 * completely filled.  Note that because the Huffman
                 * code is complete, it will always be possible to fill
                 * the sub-table eventually.
                 */
                subtable_bits = len - table_bits;
                codespace_used = count;

                while codespace_used < (1 << subtable_bits)
                {
                    subtable_bits += 1;

                    if subtable_bits + table_bits > 15
                    {
                        return Err(ZlibDecodeErrors::CorruptData);
                    }

                    codespace_used = (codespace_used << 1) + len_counts[table_bits + subtable_bits];
                }

                /*
                 * Create the entry that points from the main table to
                 * the subtable.
                 */
                decode_table[subtable_prefix] = (subtable_start as u32) << 16
                    | HUFFDEC_EXCEPTIONAL
                    | HUFFDEC_SUITABLE_POINTER
                    | (subtable_bits as u32) << 8
                    | table_bits as u32;

                curr_table_end = subtable_start + (1 << subtable_bits);
            }

            /* Fill the sub-table entries for the current codeword. */

            let stride = 1 << (len - table_bits);

            let mut j = subtable_start + (codeword >> table_bits);

            let entry = make_decode_table_entry(
                decode_results,
                sorted_syms[i] as usize,
                (len - table_bits) as u32
            );
            i += 1;

            while j < curr_table_end
            {
                decode_table[j] = entry;
                j += stride;
            }
            //advance to the next codeword
            if codeword == (1 << len) - 1
            {
                // last codeword
                return Ok(());
            }

            let adv = BITS - (codeword ^ ((1 << len) - 1)).leading_zeros();
            let bit = 1 << adv;

            codeword &= bit - 1;
            codeword |= bit;
            count -= 1;

            while count == 0
            {
                len += 1;
                count = len_counts[len];
            }
        }
    }
}

const RESIZE_BY: usize = 1024 * 4; // 4 kb

/// Resize vector if its current space wont
/// be able to store a new byte and then push an element to that new space
#[inline(always)]
fn resize_and_push(buf: &mut Vec<u8>, position: usize, elm: u8)
{
    if buf.len() <= position
    {
        let new_len = buf.len() + RESIZE_BY;
        buf.resize(new_len, 0);
    }
    buf[position] = elm;
}