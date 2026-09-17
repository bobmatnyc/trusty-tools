from routing import normalize

def test_identifier_definition():
    assert normalize('Where is resolve_chunk_file defined?') == 'fn resolve_chunk_file'
    assert normalize(' where is SOME_SYMBOL defined ') == 'fn SOME_SYMBOL'

def test_leave_other_intents_alone():
    for query in ['Where is the runtime defined?', 'Which function calls foo?', 'foo', 'Where is foo defined? plus more']:
        assert normalize(query) == query
