use std::sync::OnceLock;

use utoipa::{
    OpenApi,
    openapi::{
        Ref, RefOr,
        content::ContentBuilder,
        header::HeaderBuilder,
        response::{Response, ResponseBuilder},
        schema::{ObjectBuilder, SchemaType, Type},
    },
};
use utoipa_axum::router::OpenApiRouter;

use super::{
    api_routes,
    conversations::{DecisionAnswer, DecisionBehavior, DecisionBody, DecisionOutcomeView},
    error::{ArchiveReason, ErrorBody, ErrorCode, ErrorDetails, ErrorEnvelope, ErrorScope, Retry},
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "canterlot hub API",
        version = "1.0.0",
        description = "HTTP и WSS контракт хаба. Личность задаёт прокси перед хабом; изменяющим запросам нужны Origin, X-Canterlot-Client: 1 и Idempotency-Key."
    ),
    components(schemas(
        ErrorCode,
        ErrorScope,
        Retry,
        ErrorEnvelope,
        ErrorBody,
        ErrorDetails,
        ArchiveReason,
        DecisionBody,
        DecisionAnswer,
        DecisionBehavior,
        DecisionOutcomeView
    ))
)]
struct ApiDoc;

/// Документ собирается из тех же `OpenApiRouter`, из которых строится маршрутизатор.
pub fn document() -> utoipa::openapi::OpenApi {
    let mut base = ApiDoc::openapi();
    // Лицензия в derive по умолчанию пустая и нарушает схему OpenAPI.
    base.info.license = None;
    let mut document = OpenApiRouter::with_openapi(base)
        .merge(api_routes())
        .into_openapi();
    complete_operations(&mut document);
    document
}

/// Общие ответы и уникальные имена операций для генерации клиентских типов.
fn complete_operations(document: &mut utoipa::openapi::OpenApi) {
    let response: Response = ResponseBuilder::new()
        .description("Ошибка в формате ErrorEnvelope; код из ErrorCode, повтор определяет retry")
        .content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("ErrorEnvelope")))
                .build(),
        )
        .build();
    for (path, item) in &mut document.paths.paths {
        for (method, operation) in [
            ("get", &mut item.get),
            ("put", &mut item.put),
            ("post", &mut item.post),
            ("delete", &mut item.delete),
            ("options", &mut item.options),
            ("head", &mut item.head),
            ("patch", &mut item.patch),
            ("trace", &mut item.trace),
        ]
        .into_iter()
        .filter_map(|(method, operation)| operation.as_mut().map(|op| (method, op)))
        {
            operation.operation_id = Some(format!(
                "{method}_{}",
                path.replace(|c: char| !c.is_ascii_alphanumeric(), "_")
            ));
            operation
                .responses
                .responses
                .entry("default".to_owned())
                .or_insert_with(|| RefOr::T(response.clone()));
            for response in operation.responses.responses.values_mut() {
                if let RefOr::T(response) = response {
                    response.headers.insert(
                        "X-Server-Time-Ms".to_owned(),
                        RefOr::T(
                            HeaderBuilder::new()
                                .description(Some("Часы хаба: целые миллисекунды Unix"))
                                .schema(Some(
                                    ObjectBuilder::new()
                                        .schema_type(SchemaType::Type(Type::Integer))
                                        .minimum(Some(0)),
                                ))
                                .build(),
                        ),
                    );
                }
            }
        }
    }
}

/// Текст `docs/api/openapi.json`: красивый JSON с завершающим переводом строки.
pub fn render() -> String {
    let mut text = document()
        .to_pretty_json()
        .expect("the OpenAPI document serialises");
    text.push('\n');
    text
}

pub(crate) fn rendered() -> &'static str {
    static RENDERED: OnceLock<String> = OnceLock::new();
    RENDERED.get_or_init(render)
}
