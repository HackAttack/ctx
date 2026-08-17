use super::*;

pub(super) fn verify_body_projection(
    inverted: &InvertedIndexReader,
    analyzer: &mut tantivy::tokenizer::TextAnalyzer,
    field: Field,
    body: Option<&str>,
    doc_id: u32,
) -> Result<u64> {
    let mut expected = BTreeMap::<String, Vec<u32>>::new();
    if let Some(body) = body {
        let mut stream = analyzer.token_stream(body);
        while stream.advance() {
            let token = stream.token();
            let position = u32::try_from(token.position).map_err(|_| IndexError::CountOverflow)?;
            expected
                .entry(token.text.clone())
                .or_default()
                .push(position);
        }
    }
    let mut token_count = 0_u64;
    for (text, expected_positions) in expected {
        token_count = token_count
            .checked_add(
                u64::try_from(expected_positions.len()).map_err(|_| IndexError::CountOverflow)?,
            )
            .ok_or(IndexError::CountOverflow)?;
        let term = Term::from_field_text(field, &text);
        let term_info = inverted
            .get_term_info(&term)?
            .ok_or(IndexError::InvalidStoredDocumentField("body_search"))?;
        let mut postings = inverted
            .read_postings_from_terminfo(&term_info, IndexRecordOption::WithFreqsAndPositions)?;
        if postings.doc() > doc_id
            || postings.seek(doc_id) != doc_id
            || postings.term_freq()
                != u32::try_from(expected_positions.len()).map_err(|_| IndexError::CountOverflow)?
        {
            return Err(IndexError::InvalidStoredDocumentField("body_search"));
        }
        let mut actual_positions = Vec::with_capacity(expected_positions.len());
        postings.positions(&mut actual_positions);
        if actual_positions != expected_positions {
            return Err(IndexError::InvalidStoredDocumentField("body_search"));
        }
    }
    Ok(token_count)
}

fn live_body_token_count(searcher: &Searcher, field: Field) -> Result<u64> {
    live_body_token_count_for_segments(searcher, field, 0..searcher.segment_readers().len())
}

pub(super) fn live_body_token_count_for_segments(
    searcher: &Searcher,
    field: Field,
    segment_ordinals: impl IntoIterator<Item = usize>,
) -> Result<u64> {
    let mut total = 0_u64;
    for segment_ord in segment_ordinals {
        let segment = searcher
            .segment_readers()
            .get(segment_ord)
            .ok_or(IndexError::InvalidStoredDocumentField("body_search"))?;
        let inverted = segment.inverted_index(field)?;
        let mut terms = inverted.terms().stream()?;
        while terms.advance() {
            let mut postings = inverted
                .read_postings_from_terminfo(terms.value(), IndexRecordOption::WithFreqs)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if !segment.is_deleted(doc_id) {
                    total = total
                        .checked_add(u64::from(postings.term_freq()))
                        .ok_or(IndexError::CountOverflow)?;
                }
                doc_id = postings.advance();
            }
        }
    }
    Ok(total)
}

pub(super) fn verify_live_body_token_count(
    searcher: &Searcher,
    field: Field,
    expected: u64,
) -> Result<()> {
    if live_body_token_count(searcher, field)? != expected {
        return Err(IndexError::InvalidStoredDocumentField("body_search"));
    }
    Ok(())
}
