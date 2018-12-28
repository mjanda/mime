use std::cmp::Ordering;
use std::collections::HashMap;
use std::error::Error;
use std::hash::{Hash, Hasher};
use std::{fmt, slice};
use std::iter::Enumerate;
use std::str::Bytes;

#[derive(Clone)]
pub struct Mime {
    pub source: Source,
    pub slash: usize,
    pub plus: Option<usize>,
    pub params: ParamSource,
}

#[derive(Clone)]
pub enum Source {
    Atom(&'static str),
    Dynamic(String),
}

impl AsRef<str> for Source {
    fn as_ref(&self) -> &str {
        match *self {
            Source::Atom(s) => s,
            Source::Dynamic(ref s) => s,
        }
    }
}

type IndexedPair = (Indexed, Indexed);

#[derive(Clone)]
pub enum ParamSource {
    None,
    Utf8(usize),
    One(usize, IndexedPair),
    Two(usize, IndexedPair, IndexedPair),
    Three(usize, IndexedPair, IndexedPair, IndexedPair),
    Custom(usize, Vec<IndexedPair>),
}

#[derive(Clone, Copy)]
pub struct Indexed(usize, usize);

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
        match self {
            ParseError::MissingSlash => "a slash (/) was missing between the type and subtype",
            ParseError::MissingEqual => "an equals sign (=) was missing between a parameter and its value",
            ParseError::MissingQuote => "a quote (\") was missing from a parameter value",
            ParseError::InvalidToken { .. } => "invalid token",
            ParseError::InvalidRange => "unexpected asterisk",
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

// ===== impl Mime =====

impl Mime {
    #[inline]
    pub fn type_(&self) -> &str {
        &self.source.as_ref()[..self.slash]
    }

    #[inline]
    pub fn subtype(&self) -> &str {
        let end = self.plus.unwrap_or_else(|| {
            self.semicolon().unwrap_or_else(|| self.source.as_ref().len())
        });
        &self.source.as_ref()[self.slash + 1..end]
    }

    #[inline]
    pub fn suffix(&self) -> Option<&str> {
        let end = self.semicolon().unwrap_or_else(|| self.source.as_ref().len());
        self.plus.map(|idx| &self.source.as_ref()[idx + 1..end])
    }

    #[inline]
    pub fn params(&self) -> Params {
        let inner = match self.params {
            ParamSource::Utf8(_) => ParamsInner::Utf8,
            ParamSource::One(_, a) => ParamsInner::Inlined(&self.source, Inline::One(a)),
            ParamSource::Two(_, a, b) => ParamsInner::Inlined(&self.source, Inline::Two(a, b)),
            ParamSource::Three(_, a, b, c) => ParamsInner::Inlined(&self.source, Inline::Three(a, b, c)),
            ParamSource::Custom(_, ref params) => {
                ParamsInner::Custom {
                    source: &self.source,
                    params: params.iter(),
                }
            }
            ParamSource::None => ParamsInner::None,
        };

        Params(inner)
    }

    #[inline]
    pub fn has_params(&self) -> bool {
        self.semicolon().is_some()
    }

    #[inline]
    fn semicolon(&self) -> Option<usize> {
        match self.params {
            ParamSource::Utf8(i) |
            ParamSource::One(i, ..) |
            ParamSource::Two(i, ..) |
            ParamSource::Three(i, ..) |
            ParamSource::Custom(i, _) => Some(i),
            ParamSource::None => None,
        }
    }

    fn eq_of_params(&self, other: &Mime) -> bool {
        use self::FastEqRes::*;
        // if ParamInner is None or Utf8 we can determine equality faster
        match self.params().fast_eq(&other.params()) {
            Equals => return true,
            NotEquals => return false,
            Undetermined => {},
        }

        // OPTIMIZE: some on-stack structure might be better suited as most
        // media types do not have many parameters
        let my_params = self.params().collect::<HashMap<_,_>>();
        let other_params = self.params().collect::<HashMap<_,_>>();
        my_params == other_params
    }

    pub fn eq_str<F>(&self, s: &str, intern: F) -> bool
    where
        F: Fn(&str, usize) -> Source,
    {
        if let ParamSource::Utf8(..) = self.params {
            // this only works because ParamSource::Utf8 is only used if
            // its "<type>/<subtype>; charset=utf-8" them moment spaces are
            // set differently or charset is quoted or is utf8 it will not
            // use ParamSource::Utf8
            if self.source.as_ref().len() == s.len() {
                self.source.as_ref().eq_ignore_ascii_case(s)
            } else {
                //OPTIMIZE: once the parser is rewritten and more modular
                // we can use parts of the parser to parse the string without
                // actually crating a mime, and use that for comparision
                //
                parse(s, CanRange::Yes, intern)
                    .map(|other_mime| {
                        self == &other_mime
                    })
                    .unwrap_or(false)
            }
        } else if self.has_params() {
            parse(s, CanRange::Yes, intern)
                .map(|other_mime| {
                    self == &other_mime
                })
                .unwrap_or(false)
        } else {
            self.source.as_ref().eq_ignore_ascii_case(s)
        }
    }
}

impl PartialEq for Mime {
    #[inline]
    fn eq(&self, other: &Mime) -> bool {
        match (&self.source, &other.source) {
            (&Source::Atom(a), &Source::Atom(b)) => a == b,
            _ => {
                self.type_() == other.type_()  &&
                    self.subtype() == other.subtype() &&
                    self.suffix() == other.suffix() &&
                    self.eq_of_params(other)
            },
        }
    }
}

impl Eq for Mime {}

impl PartialOrd for Mime {
    fn partial_cmp(&self, other: &Mime) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Mime {
    fn cmp(&self, other: &Mime) -> Ordering {
        self.source.as_ref().cmp(other.source.as_ref())
    }
}

impl Hash for Mime {
    fn hash<T: Hasher>(&self, hasher: &mut T) {
        hasher.write(self.source.as_ref().as_bytes());
    }
}

impl AsRef<str> for Mime {
    #[inline]
    fn as_ref(&self) -> &str {
        self.source.as_ref()
    }
}

impl fmt::Debug for Mime {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self.source.as_ref(), f)
    }
}

impl fmt::Display for Mime {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self.source.as_ref(), f)
    }
}

#[derive(PartialEq)]
pub enum CanRange {
    Yes,
    No,
}

pub fn parse<F>(s: &str, can_range: CanRange, intern: F) -> Result<Mime, ParseError>
where
    F: Fn(&str, usize) -> Source,
{
    if s == "*/*" {
        return match can_range {
            CanRange::Yes => Ok(Mime {
                source: Source::Atom("*/*"),
                slash: 1,
                plus: None,
                params: ParamSource::None,
            }),
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
                        source: intern(s, slash),
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
                    source: intern(s, slash),
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
        ParamSource::None => intern(s, slash),
        // TODO: update intern to handle these
        ParamSource::Utf8(_) => Source::Dynamic(s.to_ascii_lowercase()),
        ParamSource::One(semicolon, a) => Source::Dynamic(lower_ascii_with_params(s, semicolon, &[a])),
        ParamSource::Two(semicolon, a, b) => Source::Dynamic(lower_ascii_with_params(s, semicolon, &[a, b])),
        ParamSource::Three(semicolon, a, b, c) => Source::Dynamic(lower_ascii_with_params(s, semicolon, &[a, b, c])),
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
                params = ParamSource::Two(semicolon, (charset, utf8), (name, value));
            },
            ParamSource::One(sc, a) => {
                params = ParamSource::Two(sc, a, (name, value));
            },
            ParamSource::Two(sc, a, b) => {
                params = ParamSource::Three(sc, a, b, (name, value));
            },
            ParamSource::Three(sc, a, b, c) => {
                params = ParamSource::Custom(sc, vec![a, b, c, (name, value)]);
            },
            ParamSource::Custom(_, ref mut vec) => {
                vec.push((name, value));
            },
            ParamSource::None => {
                if semicolon + 2 == name.0 && "charset".eq_ignore_ascii_case(&s[name.0..name.1]) &&
                    "utf-8".eq_ignore_ascii_case(&s[value.0..value.1]) {
                    params = ParamSource::Utf8(semicolon);
                    continue 'params;
                }
                params = ParamSource::One(semicolon, (name, value));
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
        if &owned[name.0..name.1] == "charset" {
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

// Params ===================


enum ParamsInner<'a> {
    Utf8,
    Inlined(&'a Source, Inline),
    Custom {
        source: &'a Source,
        params: slice::Iter<'a, IndexedPair>,
    },
    None,
}


enum Inline {
    Done,
    One(IndexedPair),
    Two(IndexedPair, IndexedPair),
    Three(IndexedPair, IndexedPair, IndexedPair),
}

enum FastEqRes {
    Equals,
    NotEquals,
    Undetermined
}

/// An iterator over the parameters of a MIME.
pub struct Params<'a>(ParamsInner<'a>);

impl<'a> fmt::Debug for Params<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("Params").finish()
    }
}

impl<'a> Params<'a> {
    fn fast_eq<'b>(&self, other: &Params<'b>) -> FastEqRes {
        match (&self.0, &other.0) {
            (&ParamsInner::None, &ParamsInner::None) |
            (&ParamsInner::Utf8, &ParamsInner::Utf8) => FastEqRes::Equals,

            (&ParamsInner::None, _) |
            (_, &ParamsInner::None)  => FastEqRes::NotEquals,

            _ => FastEqRes::Undetermined,
        }
    }
}

impl<'a> Iterator for Params<'a> {
    type Item = (&'a str, &'a str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self.0 {
            ParamsInner::Utf8 => {
                let value = ("charset", "utf-8");
                self.0 = ParamsInner::None;
                Some(value)
            },
            ParamsInner::Inlined(source, ref mut inline) => {
                let next = match *inline {
                    Inline::Done => {
                        None
                    }
                    Inline::One(one) => {
                        *inline = Inline::Done;
                        Some(one)
                    },
                    Inline::Two(one, two) => {
                        *inline = Inline::One(two);
                        Some(one)
                    },
                    Inline::Three(one, two, three) => {
                        *inline = Inline::Two(two, three);
                        Some(one)
                    },
                };
                next.map(|(name, value)| {
                    let name = &source.as_ref()[name.0..name.1];
                    let value = &source.as_ref()[value.0..value.1];
                    (name, value)
                })
            },
            ParamsInner::Custom { source, ref mut params } => {
                params.next().map(|&(name, value)| {
                    let name = &source.as_ref()[name.0..name.1];
                    let value = &source.as_ref()[value.0..value.1];
                    (name, value)
                })
            },
            ParamsInner::None => None,
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self.0 {
            ParamsInner::Utf8 => (1, Some(1)),
            ParamsInner::Inlined(_, Inline::Done) => (0, Some(0)),
            ParamsInner::Inlined(_, Inline::One(..)) => (1, Some(1)),
            ParamsInner::Inlined(_, Inline::Two(..)) => (2, Some(2)),
            ParamsInner::Inlined(_, Inline::Three(..)) => (3, Some(3)),
            ParamsInner::Custom { ref params, .. } => params.size_hint(),
            ParamsInner::None => (0, Some(0)),
        }
    }
}