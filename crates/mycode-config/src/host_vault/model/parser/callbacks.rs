//! Supplies static-error visitor callbacks for Host-vault values.

macro_rules! reject_numbers {
    () => {
        fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_i8<E>(self, _value: i8) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_i16<E>(self, _value: i16) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_i32<E>(self, _value: i32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_i128<E>(self, _value: i128) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_u8<E>(self, _value: u8) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_u16<E>(self, _value: u16) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_u32<E>(self, _value: u32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_u128<E>(self, _value: u128) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_f32<E>(self, _value: f32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_char<E>(self, _value: char) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_bytes<E>(self, _value: &[u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_borrowed_bytes<E>(self, _value: &'de [u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_byte_buf<E>(self, mut value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value.zeroize();
            Err(E::custom("vault value has wrong type"))
        }
    };
}

macro_rules! reject_non_u64_scalars {
    () => {
        fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_i8<E>(self, _value: i8) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_i16<E>(self, _value: i16) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_i32<E>(self, _value: i32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_i128<E>(self, _value: i128) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_u8<E>(self, _value: u8) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_u16<E>(self, _value: u16) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_u32<E>(self, _value: u32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_u128<E>(self, _value: u128) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_f32<E>(self, _value: f32) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_char<E>(self, _value: char) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_bytes<E>(self, _value: &[u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_borrowed_bytes<E>(self, _value: &'de [u8]) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault integer has wrong type"))
        }

        fn visit_byte_buf<E>(self, mut value: Vec<u8>) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value.zeroize();
            Err(E::custom("vault integer has wrong type"))
        }
    };
}

macro_rules! reject_strings {
    () => {
        fn visit_borrowed_str<E>(self, _value: &'de str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("non-borrowed vault strings are rejected"))
        }

        fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value.zeroize();
            Err(E::custom("owned vault strings are rejected"))
        }
    };
}

macro_rules! reject_null_and_wrappers {
    () => {
        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Err(E::custom("vault value has wrong type"))
        }

        fn visit_some<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            Err(D::Error::custom("vault value has wrong type"))
        }

        fn visit_newtype_struct<D>(self, _deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            Err(D::Error::custom("vault value has wrong type"))
        }
    };
}

macro_rules! reject_sequence {
    () => {
        fn visit_seq<A>(self, _sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            Err(A::Error::custom("vault value has wrong type"))
        }
    };
}

macro_rules! reject_map {
    () => {
        fn visit_map<A>(self, _map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            Err(A::Error::custom("vault value has wrong type"))
        }
    };
}

pub(super) use {
    reject_map, reject_non_u64_scalars, reject_null_and_wrappers, reject_numbers, reject_sequence,
    reject_strings,
};
