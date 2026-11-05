#!/usr/bin/env python3
"""Reads a SCIP index (https://github.com/scip-code/scip, scip.proto) with the standard library only.

    from scip import read_index
    index = read_index('index.scip')

The labellers run in containers that have Python but no protobuf package, and an index is read once, so a small
decoder of the fields identity labelling uses is enough. Fields not named below are skipped. The result is plain
dicts: {'documents': [Document], 'external_symbols': [SymbolInformation]}, where

- Document: relative_path, language, position_encoding, occurrences, symbols;
- Occurrence: range (0-based [line, char, end_line, end_char], from the typed single_line_range or
  multi_line_range, or the deprecated repeated range, whose three-element form is widened), symbol, symbol_roles;
- SymbolInformation: symbol, documentation (list), relationships, kind, display_name, signature (the
  signature_documentation's text, or ''), enclosing_symbol;
- Relationship: symbol, is_reference, is_implementation, is_type_definition, is_definition.

Characters in a range are counted in the document's position_encoding (scip.proto: 1 UTF-8 bytes, 2 UTF-16 code
units, 3 UTF-32 code units; 0 unspecified). byte_column() turns one into a 1-based UTF-8 byte column.
"""
from __future__ import annotations

# SymbolRole bits.
DEFINITION, IMPORT, WRITE_ACCESS, READ_ACCESS, GENERATED, TEST, FORWARD_DEFINITION = 1, 2, 4, 8, 16, 32, 64


def _varint(buf, i):
    shift = result = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if b < 0x80:
            return result, i
        shift += 7


def _fields(buf):
    """(field number, wire type, value) for each field of one message; a length-delimited value is a memoryview."""
    i, n = 0, len(buf)
    while i < n:
        key, i = _varint(buf, i)
        number, wire = key >> 3, key & 7
        if wire == 0:
            value, i = _varint(buf, i)
        elif wire == 2:
            size, i = _varint(buf, i)
            value = buf[i:i + size]
            i += size
        elif wire == 1:
            value, i = bytes(buf[i:i + 8]), i + 8
        elif wire == 5:
            value, i = bytes(buf[i:i + 4]), i + 4
        else:
            raise ValueError(f'unsupported protobuf wire type {wire}')
        yield number, wire, value


def _signed32(v):
    """An int32 field's varint as a signed number (negative int32s are encoded as 64-bit two's complement)."""
    v &= 0xFFFFFFFFFFFFFFFF
    return v - (1 << 64) if v >= 1 << 63 else v


def _ints(wire, value):
    """A repeated int32 field's values, packed or not."""
    if wire == 0:
        return [_signed32(value)]
    out, i = [], 0
    while i < len(value):
        v, i = _varint(value, i)
        out.append(_signed32(v))
    return out


def _text(value):
    return bytes(value).decode('utf-8', 'replace')


def _range(values):
    """[line, char, end_char] (one line) widened to [line, char, end_line, end_char]."""
    return [values[0], values[1], values[0], values[2]] if len(values) == 3 else values


def _relationship(buf):
    r = dict(symbol='', is_reference=False, is_implementation=False, is_type_definition=False, is_definition=False)
    names = {2: 'is_reference', 3: 'is_implementation', 4: 'is_type_definition', 5: 'is_definition'}
    for number, _, value in _fields(buf):
        if number == 1:
            r['symbol'] = _text(value)
        elif number in names:
            r[names[number]] = bool(value)
    return r


def _symbol_information(buf):
    s = dict(symbol='', documentation=[], relationships=[], kind=0, display_name='', signature='', enclosing_symbol='')
    for number, _, value in _fields(buf):
        if number == 1:
            s['symbol'] = _text(value)
        elif number == 3:
            s['documentation'].append(_text(value))
        elif number == 4:
            s['relationships'].append(_relationship(value))
        elif number == 5:
            s['kind'] = value
        elif number == 6:
            s['display_name'] = _text(value)
        elif number == 7:
            # signature_documentation is a Document; its text is field 5.
            s['signature'] = next((_text(v) for n, _, v in _fields(value) if n == 5), '')
        elif number == 8:
            s['enclosing_symbol'] = _text(value)
    return s


def _typed_range(buf, size):
    """A SingleLineRange (size 3) or MultiLineRange (size 4) message as its fields in order; proto3 omits zeros."""
    values = [0] * size
    for number, _, value in _fields(buf):
        if 1 <= number <= size:
            values[number - 1] = _signed32(value)
    return values


def _occurrence(buf):
    o = dict(range=[], symbol='', symbol_roles=0)
    for number, wire, value in _fields(buf):
        if number == 1:
            o['range'] += _ints(wire, value)
        elif number == 8:
            o['range'] = _typed_range(value, 3)
        elif number == 9:
            o['range'] = _typed_range(value, 4)
        elif number == 2:
            o['symbol'] = _text(value)
        elif number == 3:
            o['symbol_roles'] = value
    o['range'] = _range(o['range'])
    return o


def _document(buf):
    d = dict(relative_path='', language='', position_encoding=0, occurrences=[], symbols=[])
    for number, _, value in _fields(buf):
        if number == 1:
            d['relative_path'] = _text(value)
        elif number == 2:
            d['occurrences'].append(_occurrence(value))
        elif number == 3:
            d['symbols'].append(_symbol_information(value))
        elif number == 4:
            d['language'] = _text(value)
        elif number == 6:
            d['position_encoding'] = value
    return d


def parse_index(data):
    """An Index message's documents and external symbols."""
    index = dict(documents=[], external_symbols=[])
    for number, _, value in _fields(memoryview(data)):
        if number == 2:
            index['documents'].append(_document(value))
        elif number == 3:
            index['external_symbols'].append(_symbol_information(value))
    return index


def read_index(path):
    with open(path, 'rb') as f:
        return parse_index(f.read())


def byte_column(line_text, char, encoding):
    """The 1-based UTF-8 byte column of the 0-based character `char` of `line_text` (str) in a SCIP encoding."""
    if encoding == 1:
        return char + 1
    if encoding == 3:
        return len(line_text[:char].encode('utf-8')) + 1
    # UTF-16 code units, the default when unspecified (the JVM indexers count in them).
    units = i = 0
    while i < len(line_text) and units < char:
        units += 2 if ord(line_text[i]) > 0xFFFF else 1
        i += 1
    return len(line_text[:i].encode('utf-8')) + 1
