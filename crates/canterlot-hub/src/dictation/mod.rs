//! Живая диктовка: хаб пересылает звук из PWA службе `canterlot-speech`
//! по unix-сокету и возвращает распознанный текст, применяя словарь замен.

pub mod dictionary;
pub mod frame;
