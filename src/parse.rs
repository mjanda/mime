use std::error::Error;
use std::fmt;
use std::iter::Enumerate;
use std::str::Bytes;

use super::{Mime, ParamSource, Source, Indexed, CHARSET, UTF_8};

#[derive(Debug)]
pub enum ParseError {
    MissingSlash,
    MissingEqual,
    MissingQuote,
    InvalidToken {
        pos: usize,
        byte: u8,
    },
    InvalidRange,
}

impl Error for ParseError {
    fn description(&self) -> &str {
        use self::ParseError::*;

        match *self {
            MissingSlash => "a slash (/) was missing between the type and subtype",
            MissingEqual => "an equals sign (=) was missing between a parameter and its value",
            MissingQuote => "a quote (\") was missing from a parameter value",
            InvalidToken { .. } => "an invalid token was encountered",
            InvalidRange => "unexpected asterisk",
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let ParseError::InvalidToken { pos, byte } = *self {
            write!(f, "{}, {:X} at position {}", self.description(), byte, pos)
        } else {
            f.write_str(self.description())
        }
    }
}

#[derive(PartialEq)]
pub(super) enum CanRange {
    Yes,
    No,
}

pub(super) fn parse(s: &str, can_range: CanRange) -> Result<Mime, ParseError> {
    if s == "*/*" {
        return match can_range {
            CanRange::Yes => Ok(crate::MIME_STAR_STAR),
            CanRange::No => Err(ParseError::InvalidRange),
        };
    }

    let mut iter = s.bytes().enumerate();
    // toplevel
    let mut start;
    let slash;
    loop {
        match iter.next() {
            Some((_, c)) if is_token(c) => (),
            Some((i, b'/')) if i > 0 => {
                slash = i;
                start = i + 1;
                break;
            },
            None => return Err(ParseError::MissingSlash), // EOF and no toplevel is no Mime
            Some((pos, byte)) => return Err(ParseError::InvalidToken {
                pos: pos,
                byte: byte,
            }),
        };
    }

    // sublevel
    let mut plus = None;
    loop {
        match iter.next() {
            Some((i, b'+')) if i > start => {
                plus = Some(i);
            },
            Some((i, b';')) if i > start => {
                start = i;
                break;
            },

            Some((i, b'*')) if i == start && can_range == CanRange::Yes => {
                // sublevel star can only be the first character, and the next
                // must either be the end, or `;`
                match iter.next() {
                    Some((i, b';')) => {
                        start = i;
                        break;
                    },
                    None => return Ok(Mime {
                        source: Source::intern(s, slash),
                        slash,
                        plus,
                        params: ParamSource::None,
                    }),
                    Some((pos, byte)) => return Err(ParseError::InvalidToken {
                        pos,
                        byte,
                    }),
                }
            },

            Some((_, c)) if is_token(c) => (),
            None => {
                return Ok(Mime {
                    source: Source::intern(s, slash),
                    slash,
                    plus,
                    params: ParamSource::None,
                });
            },
            Some((pos, byte)) => return Err(ParseError::InvalidToken {
                pos: pos,
                byte: byte,
            })
        };
    }

    // params
    let params = params_from_str(s, &mut iter, start)?;

    let source = match params {
        ParamSource::None => Source::intern(s, slash),
        // TODO: update intern to handle these
        ParamSource::Utf8(_) => Source::Dynamic(s.to_ascii_lowercase()),
        ParamSource::Custom(semicolon, ref indices) => Source::Dynamic(lower_ascii_with_params(s, semicolon, indices)),
    };

    Ok(Mime {
        source,
        slash,
        plus,
        params,
    })
}


fn params_from_str(s: &str, iter: &mut Enumerate<Bytes>, mut start: usize) -> Result<ParamSource, ParseError> {
    let semicolon = start;
    start += 1;
    let mut params = ParamSource::None;
    'params: while start < s.len() {
        let name;
        // name
        'name: loop {
            match iter.next() {
                Some((i, b' ')) if i == start => start = i + 1,
                Some((_, c)) if is_token(c) => (),
                Some((i, b'=')) if i > start => {
                    name = Indexed(start, i);
                    start = i + 1;
                    break 'name;
                },
                None => return Err(ParseError::MissingEqual),
                Some((pos, byte)) => return Err(ParseError::InvalidToken {
                    pos: pos,
                    byte: byte,
                }),
            }
        }

        let value;
        // values must be restrict-name-char or "anything goes"
        let mut is_quoted = false;
        let mut is_quoted_pair = false;

        'value: loop {
            if is_quoted {
                if is_quoted_pair {
                    is_quoted_pair = false;
                    match iter.next() {
                        Some((_, ch)) if is_restricted_quoted_char(ch) => (),
                        Some((pos, byte)) => return Err(ParseError::InvalidToken {
                            pos: pos,
                            byte: byte,
                        }),
                        None => return Err(ParseError::MissingQuote),
                    }

                } else {
                    match iter.next() {
                        Some((i, b'"')) if i > start => {
                            value = Indexed(start, i+1);
                            break 'value;
                        },
                        Some((_, b'\\')) => is_quoted_pair = true,
                        Some((_, c)) if is_restricted_quoted_char(c) => (),
                        None => return Err(ParseError::MissingQuote),
                        Some((pos, byte)) => return Err(ParseError::InvalidToken {
                            pos: pos,
                            byte: byte,
                        }),
                    }
                }
            } else {
                match iter.next() {
                    Some((i, b'"')) if i == start => {
                        is_quoted = true;
                        start = i;
                    },
                    Some((_, c)) if is_token(c) => (),
                    Some((i, b';')) if i > start => {
                        value = Indexed(start, i);
                        start = i + 1;
                        break 'value;
                    }
                    None => {
                        value = Indexed(start, s.len());
                        start = s.len();
                        break 'value;
                    },

                    Some((pos, byte)) => return Err(ParseError::InvalidToken {
                        pos: pos,
                        byte: byte,
                    }),
                }
            }
        }

        if is_quoted {
            'ws: loop {
                match iter.next() {
                    Some((i, b';')) => {
                        // next param
                        start = i + 1;
                        break 'ws;
                    },
                    Some((_, b' ')) => {
                        // skip whitespace
                    },
                    None => {
                        // eof
                        start = s.len();
                        break 'ws;
                    },
                    Some((pos, byte)) => return Err(ParseError::InvalidToken {
                        pos: pos,
                        byte: byte,
                    }),
                }
            }
        }

        match params {
            ParamSource::Utf8(i) => {
                let i = i + 2;
                let charset = Indexed(i, "charset".len() + i);
                let utf8 = Indexed(charset.1 + 1, charset.1 + "utf-8".len() + 1);
                params = ParamSource::Custom(semicolon, vec![
                    (charset, utf8),
                    (name, value),
                ]);
            },
            ParamSource::Custom(_, ref mut vec) => {
                vec.push((name, value));
            },
            ParamSource::None => {
                if semicolon + 2 == name.0 && CHARSET == s[name.0..name.1] &&
                    UTF_8 == s[value.0..value.1] {
                    params = ParamSource::Utf8(semicolon);
                    continue 'params;
                }
                params = ParamSource::Custom(semicolon, vec![(name, value)]);
            },
        }
    }
    Ok(params)
}

fn lower_ascii_with_params(s: &str, semi: usize, params: &[(Indexed, Indexed)]) -> String {
    let mut owned = s.to_owned();
    owned[..semi].make_ascii_lowercase();

    for &(ref name, ref value) in params {
        owned[name.0..name.1].make_ascii_lowercase();
        // Since we just converted this part of the string to lowercase,
        // we can skip the `Name == &str` unicase check and do a faster
        // memcmp instead.
        if &owned[name.0..name.1] == CHARSET.as_str() {
            owned[value.0..value.1].make_ascii_lowercase();
        }
    }

    owned
}

// From [RFC6838](http://tools.ietf.org/html/rfc6838#section-4.2):
//
// > All registered media types MUST be assigned top-level type and
// > subtype names.  The combination of these names serves to uniquely
// > identify the media type, and the subtype name facet (or the absence
// > of one) identifies the registration tree.  Both top-level type and
// > subtype names are case-insensitive.
// >
// > Type and subtype names MUST conform to the following ABNF:
// >
// >     type-name = restricted-name
// >     subtype-name = restricted-name
// >
// >     restricted-name = restricted-name-first *126restricted-name-chars
// >     restricted-name-first  = ALPHA / DIGIT
// >     restricted-name-chars  = ALPHA / DIGIT / "!" / "#" /
// >                              "$" / "&" / "-" / "^" / "_"
// >     restricted-name-chars =/ "." ; Characters before first dot always
// >                                  ; specify a facet name
// >     restricted-name-chars =/ "+" ; Characters after last plus always
// >                                  ; specify a structured syntax suffix

// However, [HTTP](https://tools.ietf.org/html/rfc7231#section-3.1.1.1):
//
// >     media-type = type "/" subtype *( OWS ";" OWS parameter )
// >     type       = token
// >     subtype    = token
// >     parameter  = token "=" ( token / quoted-string )
//
// Where token is defined as:
//
// >     token = 1*tchar
// >     tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
// >        "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
//
// So, clearly, ¯\_(Ä_/¯

macro_rules! byte_map {
    ($($flag:expr,)*) => ([
        $($flag != 0,)*
    ])
}

static TOKEN_MAP: [bool; 256] = byte_map![
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 1, 0, 1, 1, 1, 1, 1, 0, 0, 0, 1, 0, 1, 1, 0,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0,
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 0, 1, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

fn is_token(c: u8) -> bool {
    TOKEN_MAP[c as usize]
}

fn is_restricted_quoted_char(c: u8) -> bool {
    c == 9 || (c > 31 && c != 127)
}

#[test]
fn test_lookup_tables() {
    for (i, &valid) in TOKEN_MAP.iter().enumerate() {
        let i = i as u8;
        let should = match i {
            b'a'...b'z' |
            b'A'...b'Z' |
            b'0'...b'9' |
            b'!' |
            b'#' |
            b'$' |
            b'%' |
            b'&' |
            b'\'' |
            b'+' |
            b'-' |
            b'.' |
            b'^' |
            b'_' |
            b'`' |
            b'|' |
            b'~' => true,
            _ => false
        };
        assert_eq!(valid, should, "{:?} ({}) should be {}", i as char, i, should);
    }
}
