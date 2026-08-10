//! **Terms of consignment**: the document the holder signs when they receive a
//! key, generated from the record rather than retyped, in the language the holder
//! reads.
//!
//! Three parts:
//!
//! * [`TermTemplate`] — a titled body with `{{variables}}`, identified by
//!   `(id, language, version)`. Editing a term produces a new version; the
//!   version that was signed stays readable forever.
//! * [`TermContext`] — everything a term can say, gathered from the holder, the
//!   key, the bootstrap run and the operator.
//! * [`render_term`] — substitution plus **line omission**: a line whose
//!   placeholder resolves to empty disappears, so a holder with no phone number
//!   does not get a stray "Phone:" line. That is the whole conditional logic, and
//!   it is enough for a document of this kind.
//!
//! The signed document itself is attached to the distribution — see
//! [`crate::domain::document`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{DistributionRecord, Holder, YubiKeyRecord};

/// Language tag of a term. Free-form (BCP 47) so a unit can add its own.
pub const DEFAULT_LANGUAGE: &str = "pt-BR";

/// Languages shipped with the application.
pub const BUILTIN_LANGUAGES: [&str; 2] = ["pt-BR", "en"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TermError {
    #[error("unknown variable `{0}` in the term template")]
    UnknownVariable(String),
    #[error("unterminated `{{{{` in the term template")]
    Unterminated,
    #[error("no term template for language `{0}`")]
    NoTemplate(String),
    #[error("the term is missing required information: {0}")]
    Incomplete(&'static str),
}

/// A term template in one language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermTemplate {
    /// Stable id shared across languages, e.g. `consignment`.
    pub id: String,
    /// BCP 47 language tag, e.g. `pt-BR`.
    pub language: String,
    pub version: String,
    /// Rendered as the document heading.
    pub title: String,
    /// The document, with `{{variables}}`.
    pub body: String,
}

impl TermTemplate {
    /// `consignment` in Brazilian Portuguese — the primary language here.
    pub fn consignment_pt_br() -> Self {
        Self {
            id: "consignment".into(),
            language: "pt-BR".into(),
            version: "1".into(),
            title: "Termo de Consignação de Chave de Segurança".into(),
            body: PT_BR_BODY.into(),
        }
    }

    /// `consignment` in English, for holders who need it.
    pub fn consignment_en() -> Self {
        Self {
            id: "consignment".into(),
            language: "en".into(),
            version: "1".into(),
            title: "Security Key Consignment Term".into(),
            body: EN_BODY.into(),
        }
    }

    pub fn builtin() -> Vec<Self> {
        vec![Self::consignment_pt_br(), Self::consignment_en()]
    }

    /// Variables the body references, in order of first appearance. Used by the
    /// editor to show what a template depends on, and by `validate`.
    pub fn referenced_variables(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut rest = self.body.as_str();
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else { break };
            let name = after[..end].trim().to_owned();
            if !name.is_empty() && !found.contains(&name) {
                found.push(name);
            }
            rest = &after[end + 2..];
        }
        found
    }

    /// Check that every variable the body uses is one the context can supply.
    pub fn validate(&self) -> Result<(), TermError> {
        let known = TermContext::VARIABLES;
        for name in self.referenced_variables() {
            if !known.contains(&name.as_str()) {
                return Err(TermError::UnknownVariable(name));
            }
        }
        Ok(())
    }
}

/// Everything a term can interpolate.
///
/// Optional values are empty strings, and [`render_term`] drops the lines that
/// depend on them — which is how "phone" and "address" stay optional without
/// every template needing a conditional.
#[derive(Debug, Clone, Default)]
pub struct TermContext {
    pub holder_name: String,
    /// CPF or equivalent. Called an *identification number* throughout, because
    /// this record is not limited to Brazilian documents.
    pub holder_identification: String,
    pub holder_email: String,
    pub holder_unit: String,
    pub holder_registration: String,
    pub holder_phone: String,
    pub holder_address: String,
    pub key_serial: String,
    pub key_model: String,
    pub key_firmware: String,
    /// What the bootstrap applied, e.g. `fgv-standard 1 — FIDO2 PIN, …`.
    pub applied: String,
    /// The custody statement the holder is agreeing to.
    pub custody: String,
    pub operator: String,
    pub org: String,
    pub org_unit: String,
    /// `dd/mm/yyyy`, matching the log format the norm specifies.
    pub date: String,
    pub delivery_method: String,
    pub receipt_ref: String,
}

impl TermContext {
    /// Every name a term template may use.
    pub const VARIABLES: [&'static str; 18] = [
        "holder.name",
        "holder.identification",
        "holder.email",
        "holder.unit",
        "holder.registration",
        "holder.phone",
        "holder.address",
        "key.serial",
        "key.model",
        "key.firmware",
        "applied",
        "custody",
        "operator",
        "org",
        "org.unit",
        "date",
        "delivery.method",
        "receipt.ref",
    ];

    /// Build a context from the records. `applied` and `custody` come from the
    /// bootstrap run, which the caller resolves.
    pub fn from_records(
        holder: &Holder,
        key: &YubiKeyRecord,
        distribution: Option<&DistributionRecord>,
        applied: &str,
        custody: &str,
        operator: &str,
        org: &str,
    ) -> Self {
        Self {
            holder_name: holder.full_name.clone(),
            holder_identification: holder.identification_number.clone(),
            holder_email: holder.email.clone(),
            holder_unit: holder.unit.clone(),
            holder_registration: holder.registration.clone(),
            holder_phone: holder.phone.clone(),
            holder_address: holder.address.clone(),
            key_serial: key.serial.to_string(),
            key_model: key.model.clone(),
            key_firmware: key.firmware.clone(),
            applied: applied.to_owned(),
            custody: custody.to_owned(),
            operator: operator.to_owned(),
            org: org.to_owned(),
            org_unit: holder.unit.clone(),
            date: chrono::Local::now().format("%d/%m/%Y").to_string(),
            delivery_method: distribution
                .map(|d| d.method.label().to_owned())
                .unwrap_or_default(),
            receipt_ref: distribution
                .map(|d| d.receipt_ref.clone())
                .unwrap_or_default(),
        }
    }

    fn lookup(&self, name: &str) -> Option<&str> {
        Some(match name {
            "holder.name" => &self.holder_name,
            "holder.identification" => &self.holder_identification,
            "holder.email" => &self.holder_email,
            "holder.unit" => &self.holder_unit,
            "holder.registration" => &self.holder_registration,
            "holder.phone" => &self.holder_phone,
            "holder.address" => &self.holder_address,
            "key.serial" => &self.key_serial,
            "key.model" => &self.key_model,
            "key.firmware" => &self.key_firmware,
            "applied" => &self.applied,
            "custody" => &self.custody,
            "operator" => &self.operator,
            "org" => &self.org,
            "org.unit" => &self.org_unit,
            "date" => &self.date,
            "delivery.method" => &self.delivery_method,
            "receipt.ref" => &self.receipt_ref,
            _ => return None,
        })
    }

    /// The facts a term cannot be issued without.
    pub fn check_required(&self) -> Result<(), TermError> {
        if self.holder_name.trim().is_empty() {
            return Err(TermError::Incomplete("the holder's name"));
        }
        if self.key_serial.trim().is_empty() {
            return Err(TermError::Incomplete("the key serial"));
        }
        Ok(())
    }

    /// Values as a map, for an editor's preview panel.
    pub fn as_map(&self) -> BTreeMap<&'static str, String> {
        Self::VARIABLES
            .into_iter()
            .map(|name| (name, self.lookup(name).unwrap_or_default().to_owned()))
            .collect()
    }
}

/// Render a term.
///
/// Substitution is per line, and **a line that uses a variable which resolves to
/// empty is omitted entirely**. So a template can carry
/// `Telefone: {{holder.phone}}` unconditionally: the line appears for a holder who
/// gave a phone number and vanishes for one who did not. A line with no variables
/// is always kept.
pub fn render_term(template: &TermTemplate, ctx: &TermContext) -> Result<String, TermError> {
    ctx.check_required()?;

    let mut out = String::with_capacity(template.body.len());
    out.push_str(&render_line(&template.title, ctx)?.unwrap_or_default());
    out.push_str("\n\n");

    for line in template.body.lines() {
        match render_line(line, ctx)? {
            Some(rendered) => {
                out.push_str(&rendered);
                out.push('\n');
            }
            // The line depended on something the holder did not provide.
            None => continue,
        }
    }

    Ok(out)
}

/// Render one line, returning `None` when it should be dropped.
fn render_line(line: &str, ctx: &TermContext) -> Result<Option<String>, TermError> {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut saw_variable = false;
    let mut saw_empty = false;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(TermError::Unterminated);
        };
        let name = after[..end].trim();
        let value = ctx
            .lookup(name)
            .ok_or_else(|| TermError::UnknownVariable(name.to_owned()))?;

        saw_variable = true;
        if value.trim().is_empty() {
            saw_empty = true;
        }
        out.push_str(value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);

    if saw_variable && saw_empty {
        return Ok(None);
    }
    Ok(Some(out))
}

/// Pick the best template for a wanted language.
///
/// Exact match first, then the base language (`pt-BR` satisfies a request for
/// `pt`), then the default language, then any template at all — a term in the
/// wrong language is better than no term, and the caller is told which was used.
pub fn choose_template<'a>(
    templates: &'a [TermTemplate],
    id: &str,
    wanted: &str,
) -> Option<&'a TermTemplate> {
    let of_id: Vec<&TermTemplate> = templates.iter().filter(|t| t.id == id).collect();
    if of_id.is_empty() {
        return None;
    }

    let wanted_base = wanted.split('-').next().unwrap_or(wanted);

    of_id
        .iter()
        .find(|t| t.language.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            of_id.iter().find(|t| {
                t.language
                    .split('-')
                    .next()
                    .unwrap_or(&t.language)
                    .eq_ignore_ascii_case(wanted_base)
            })
        })
        .or_else(|| {
            of_id
                .iter()
                .find(|t| t.language.eq_ignore_ascii_case(DEFAULT_LANGUAGE))
        })
        .or(of_id.first())
        .copied()
}

const PT_BR_BODY: &str = r#"{{org}} — {{org.unit}}
Data: {{date}}

1. IDENTIFICAÇÃO DO PORTADOR

Nome: {{holder.name}}
Número de identificação: {{holder.identification}}
E-mail: {{holder.email}}
Lotação: {{holder.unit}}
Matrícula: {{holder.registration}}
Telefone: {{holder.phone}}
Endereço: {{holder.address}}

2. IDENTIFICAÇÃO DA CHAVE

Número de série: {{key.serial}}
Modelo: {{key.model}}
Versão de firmware: {{key.firmware}}
Configuração aplicada: {{applied}}
Forma de entrega: {{delivery.method}}
Referência do termo: {{receipt.ref}}

3. CUSTÓDIA DAS SENHAS

{{custody}}

O PIN entregue com a chave é provisório e deve ser substituído pelo portador no
primeiro uso. O PIN definido pelo portador é pessoal e não deve ser compartilhado
com ninguém, incluindo a equipe de tecnologia.

4. RESPONSABILIDADES DO PORTADOR

4.1. A chave é um dispositivo de identificação pessoal e intransferível, e seu uso
     equivale à assinatura do portador.
4.2. O portador compromete-se a guardar a chave com o mesmo cuidado dedicado a um
     documento de identidade.
4.3. A perda, o furto, o extravio ou a suspeita de comprometimento devem ser
     comunicados imediatamente à unidade responsável, para revogação dos
     certificados e das credenciais associadas.
4.4. A chave deve ser devolvida em caso de desligamento, mudança de função ou
     quando solicitado pela unidade responsável.
4.5. O portador declara ter recebido orientação sobre o uso da chave e sobre a
     troca do PIN provisório.

5. TRATAMENTO DE DADOS PESSOAIS

Os dados pessoais informados neste termo são tratados exclusivamente para o
controle de distribuição e para a emissão do certificado de assinatura vinculado
ao portador, conforme a Lei nº 13.709/2018 (LGPD).

6. ASSINATURAS

Entregue por: {{operator}}


_______________________________          _______________________________
{{holder.name}}                          {{operator}}
Portador                                 Responsável pela entrega
"#;

const EN_BODY: &str = r#"{{org}} — {{org.unit}}
Date: {{date}}

1. HOLDER

Name: {{holder.name}}
Identification number: {{holder.identification}}
E-mail: {{holder.email}}
Unit: {{holder.unit}}
Registration: {{holder.registration}}
Phone: {{holder.phone}}
Address: {{holder.address}}

2. SECURITY KEY

Serial number: {{key.serial}}
Model: {{key.model}}
Firmware version: {{key.firmware}}
Applied configuration: {{applied}}
Delivery method: {{delivery.method}}
Term reference: {{receipt.ref}}

3. CUSTODY OF THE PINS

{{custody}}

The PIN handed over with the key is temporary and must be replaced by the holder
on first use. The PIN chosen by the holder is personal and must not be shared with
anyone, including technology staff.

4. THE HOLDER'S RESPONSIBILITIES

4.1. The key is a personal, non-transferable identification device, and its use is
     equivalent to the holder's signature.
4.2. The holder undertakes to keep the key with the same care given to an identity
     document.
4.3. Loss, theft, misplacement or any suspicion of compromise must be reported to
     the responsible unit immediately, so that the associated certificates and
     credentials can be revoked.
4.4. The key must be returned on termination, on a change of role, or whenever the
     responsible unit requests it.
4.5. The holder confirms having received guidance on using the key and on replacing
     the temporary PIN.

5. PERSONAL DATA

The personal data in this term is processed solely to control key distribution and
to issue the signing certificate bound to the holder.

6. SIGNATURES

Handed over by: {{operator}}


_______________________________          _______________________________
{{holder.name}}                          {{operator}}
Holder                                   Issuing operator
"#;
