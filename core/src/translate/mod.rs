mod ast_literal;
mod data_type;
mod ddl;
mod error;
mod expr;
mod function;
mod operator;
mod query;

pub use self::{
    data_type::translate_data_type,
    ddl::translate_column_def,
    error::TranslateError,
    expr::{translate_expr, translate_order_by_expr},
    query::{alias_or_name, translate_query, translate_select_item},
};

use {
    crate::{
        ast::{Assignment, Statement, Variable},
        result::Result,
    },
    ddl::translate_alter_table_operation,
    sqlparser::ast::{
        Assignment as SqlAssignment, Ident as SqlIdent, ObjectName as SqlObjectName,
        ObjectType as SqlObjectType, Statement as SqlStatement, TableFactor, TableWithJoins,
    },
};

pub fn translate(sql_statement: &SqlStatement) -> Result<Statement> {
    match sql_statement {
        SqlStatement::Query(query) => translate_query(query).map(Statement::Query),
        SqlStatement::Insert {
            table_name,
            columns,
            source,
            ..
        } => Ok(Statement::Insert {
            table_name: translate_object_name(table_name)?,
            columns: translate_idents(columns),
            source: translate_query(source)?,
        }),
        SqlStatement::Update {
            table,
            assignments,
            selection,
            ..
        } => Ok(Statement::Update {
            table_name: translate_table_with_join(table)?,
            assignments: assignments
                .iter()
                .map(translate_assignment)
                .collect::<Result<_>>()?,
            selection: selection.as_ref().map(translate_expr).transpose()?,
        }),
        SqlStatement::Delete {
            table_name: TableFactor::Table {
                name: table_name, ..
            },
            selection,
            ..
        } => Ok(Statement::Delete {
            table_name: translate_object_name(table_name)?,
            selection: selection.as_ref().map(translate_expr).transpose()?,
        }),
        SqlStatement::CreateTable {
            if_not_exists,
            name,
            columns,
            query,
            engine,
            ..
        } => {
            let columns = columns
                .iter()
                .map(translate_column_def)
                .collect::<Result<Vec<_>>>()?;

            let columns = (!columns.is_empty()).then_some(columns);

            Ok(Statement::CreateTable {
                if_not_exists: *if_not_exists,
                name: translate_object_name(name)?,
                columns,
                source: match query {
                    Some(v) => Some(translate_query(v).map(Box::new)?),
                    None => None,
                },
                engine: engine.clone(),
            })
        }
        SqlStatement::AlterTable {
            name, operation, ..
        } => Ok(Statement::AlterTable {
            name: translate_object_name(name)?,
            operation: translate_alter_table_operation(operation)?,
        }),
        SqlStatement::Drop {
            object_type: SqlObjectType::Table,
            if_exists,
            names,
            ..
        } => Ok(Statement::DropTable {
            if_exists: *if_exists,
            names: names
                .iter()
                .map(translate_object_name)
                .collect::<Result<Vec<_>>>()?,
        }),
        SqlStatement::CreateIndex {
            name,
            table_name,
            columns,
            ..
        } => {
            if columns.len() > 1 {
                return Err(TranslateError::CompositeIndexNotSupported.into());
            }

            let name = translate_object_name(name)?;

            if name.to_uppercase() == "PRIMARY" {
                return Err(TranslateError::ReservedIndexName(name).into());
            };

            Ok(Statement::CreateIndex {
                name,
                table_name: translate_object_name(table_name)?,
                column: translate_order_by_expr(&columns[0])?,
            })
        }
        SqlStatement::Drop {
            object_type: SqlObjectType::Index,
            names,
            ..
        } => {
            if names.len() > 1 {
                return Err(TranslateError::TooManyParamsInDropIndex.into());
            }

            let object_name = &names[0].0;
            if object_name.len() != 2 {
                return Err(TranslateError::InvalidParamsInDropIndex.into());
            }

            let table_name = object_name[0].value.to_owned();
            let name = object_name[1].value.to_owned();

            if name.to_uppercase() == "PRIMARY" {
                return Err(TranslateError::CannotDropPrimary.into());
            };

            Ok(Statement::DropIndex { name, table_name })
        }
        SqlStatement::StartTransaction { .. } => Ok(Statement::StartTransaction),
        SqlStatement::Commit { .. } => Ok(Statement::Commit),
        SqlStatement::Rollback { .. } => Ok(Statement::Rollback),
        SqlStatement::ShowTables {
            filter: None,
            db_name: None,
            ..
        } => Ok(Statement::ShowVariable(Variable::Tables)),
        SqlStatement::ShowVariable { variable } => match (variable.len(), variable.get(0)) {
            (1, Some(keyword)) => match keyword.value.to_uppercase().as_str() {
                "VERSION" => Ok(Statement::ShowVariable(Variable::Version)),
                v => Err(TranslateError::UnsupportedShowVariableKeyword(v.to_owned()).into()),
            },
            (3, Some(keyword)) => match keyword.value.to_uppercase().as_str() {
                "INDEXES" => match variable.get(2) {
                    Some(tablename) => Ok(Statement::ShowIndexes(tablename.value.to_owned())),
                    _ => Err(TranslateError::UnsupportedShowVariableStatement(
                        sql_statement.to_string(),
                    )
                    .into()),
                },
                _ => Err(TranslateError::UnsupportedShowVariableStatement(
                    sql_statement.to_string(),
                )
                .into()),
            },
            _ => Err(
                TranslateError::UnsupportedShowVariableStatement(sql_statement.to_string()).into(),
            ),
        },
        SqlStatement::ShowColumns { table_name, .. } => Ok(Statement::ShowColumns {
            table_name: translate_object_name(table_name)?,
        }),
        _ => Err(TranslateError::UnsupportedStatement(sql_statement.to_string()).into()),
    }
}

pub fn translate_assignment(sql_assignment: &SqlAssignment) -> Result<Assignment> {
    let SqlAssignment { id, value } = sql_assignment;

    if id.len() > 1 {
        return Err(
            TranslateError::CompoundIdentOnUpdateNotSupported(sql_assignment.to_string()).into(),
        );
    }

    Ok(Assignment {
        id: id
            .get(0)
            .ok_or(TranslateError::UnreachableEmptyIdent)?
            .value
            .to_owned(),
        value: translate_expr(value)?,
    })
}

fn translate_table_with_join(table: &TableWithJoins) -> Result<String> {
    if !table.joins.is_empty() {
        return Err(TranslateError::JoinOnUpdateNotSupported.into());
    }
    match &table.relation {
        TableFactor::Table { name, .. } => translate_object_name(name),
        t => Err(TranslateError::UnsupportedTableFactor(t.to_string()).into()),
    }
}

fn translate_object_name(sql_object_name: &SqlObjectName) -> Result<String> {
    let sql_object_name = &sql_object_name.0;
    if sql_object_name.len() > 1 {
        let compound_object_name = translate_idents(sql_object_name).join(".");
        return Err(TranslateError::CompoundObjectNotSupported(compound_object_name).into());
    }

    sql_object_name
        .get(0)
        .map(|v| v.value.to_owned())
        .ok_or_else(|| TranslateError::UnreachableEmptyObject.into())
}

pub fn translate_idents(idents: &[SqlIdent]) -> Vec<String> {
    idents.iter().map(|v| v.value.to_owned()).collect()
}
