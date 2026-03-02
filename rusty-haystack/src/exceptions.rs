// Custom Python exception types for Haystack operations.

use pyo3::create_exception;
use pyo3::exceptions::PyException;

create_exception!(
    rusty_haystack,
    HaystackError,
    PyException,
    "Base exception for all Haystack errors."
);
create_exception!(
    rusty_haystack,
    CodecError,
    HaystackError,
    "Error encoding or decoding Haystack data."
);
create_exception!(
    rusty_haystack,
    FilterError,
    HaystackError,
    "Error parsing or evaluating a filter expression."
);
create_exception!(
    rusty_haystack,
    GraphError,
    HaystackError,
    "Error performing a graph operation."
);
create_exception!(
    rusty_haystack,
    AuthError,
    HaystackError,
    "Error during SCRAM authentication."
);
create_exception!(
    rusty_haystack,
    ClientError,
    HaystackError,
    "Error in the Haystack client."
);
