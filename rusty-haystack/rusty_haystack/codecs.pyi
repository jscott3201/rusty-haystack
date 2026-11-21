from . import HGrid, _InputValue, _Value

def encode_grid(codec_name: str, grid: HGrid) -> str:
    """Encode an HGrid to a string using the specified codec.

    Supported codecs: 'text/zinc', 'application/json', 'application/json;v=3',
    'text/trio', 'text/csv'.
    """
    ...


def decode_grid(codec_name: str, data: str) -> HGrid:
    """Decode a string to an HGrid using the specified codec."""
    ...


def encode_scalar(codec_name: str, val: _InputValue) -> str:
    """Encode a scalar Haystack value to a string."""
    ...


def decode_scalar(codec_name: str, data: str) -> _Value:
    """Decode a string to a scalar Haystack value."""
    ...


def encode_typed_value(value: _InputValue) -> str: ...
def decode_typed_value(payload: str) -> _Value: ...
