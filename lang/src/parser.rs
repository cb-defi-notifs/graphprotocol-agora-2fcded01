use crate::{expressions::*, language::*};
use graphql_parser::{
    consume_query,
    query::{Definition, OperationDefinition},
};
use nom::{
    branch::alt,
    bytes::complete::{tag, take_while1},
    character::complete::{alpha1, alphanumeric1, digit1},
    combinator::{map, opt, recognize},
    error::{ErrorKind, ParseError},
    multi::many0,
    sequence::tuple,
    sequence::{preceded, terminated},
    Err as NomErr, IResult, InputTakeAtPosition,
};
// TODO: Switch to fraction::BigFraction
use num_bigint::BigInt;

fn graphql_query<'a>(input: &'a str) -> IResult<&'a str, TopLevelQueryItem<'a>> {
    let (query, input) =
        consume_query(input).map_err(|_| NomErr::Error((input, ErrorKind::Verify)))?;
    let query = match query {
        Definition::Operation(OperationDefinition::Query(query)) => query,
        _ => return Err(NomErr::Error((input, ErrorKind::Verify))),
    };

    if query.name.is_some() {
        return Err(NomErr::Error((input, ErrorKind::Verify)));
    }
    if query.variable_definitions.len() != 0 {
        return Err(NomErr::Error((input, ErrorKind::Verify)));
    }

    let mut directives = query.directives;
    let mut selection = query.selection_set.items;

    // TODO: Use single crate here (Bug - can have multiple items)
    match (directives.pop(), selection.pop()) {
        (None, Some(selection)) => Ok((input, TopLevelQueryItem::Selection(selection))),
        (Some(directive), None) => Ok((input, TopLevelQueryItem::Directive(directive))),
        _ => return Err(NomErr::Error((input, ErrorKind::Verify))),
    }
}

fn whitespace<I: Clone>(input: I) -> IResult<I, I>
where
    I: InputTakeAtPosition<Item = char>,
{
    let is_whitespace = |c| c == ' ' || c == '\t' || c == '\r' || c == '\n';
    take_while1(is_whitespace)(input)
}

fn where_clause(input: &str) -> IResult<&str, WhereClause> {
    let (input, condition) = preceded(tuple((tag("where"), whitespace)), condition)(input)?;
    Ok((input, WhereClause { condition }))
}

fn const_bool(input: &str) -> IResult<&str, Const<bool>> {
    let (input, value) = alt((map(tag("true"), |_| true), map(tag("false"), |_| false)))(input)?;
    Ok((input, Const::new(value)))
}

// TODO: (Security) Ensure a recursion limit
fn condition_leaf(input: &str) -> IResult<&str, Condition> {
    alt((
        |input| parenthesized(condition, input),
        map(comparison, Condition::Comparison),
        map(variable, Condition::Variable),
        map(const_bool, Condition::Const),
    ))(input)
}

fn condition(input: &str) -> IResult<&str, Condition> {
    let (input, mut first) = condition_leaf(input)?;
    let (input, ops) = many0(tuple((
        surrounded_by(whitespace, any_boolean_operator),
        condition_leaf,
    )))(input)?;

    for (op, expr) in ops.into_iter() {
        first = Condition::Boolean(Box::new(BinaryExpression::new(first, op, expr)));
    }

    Ok((input, first))
}

fn comparison(input: &str) -> IResult<&str, BinaryExpression<AnyComparison, LinearExpression>> {
    let (input, lhs) = linear_expression(input)?;
    let (input, op) = surrounded_by(
        opt(whitespace),
        alt((
            |input| binary_operator(input, "==", Eq),
            |input| binary_operator(input, "!=", Ne),
            |input| binary_operator(input, ">=", Ge),
            |input| binary_operator(input, "<=", Le),
            |input| binary_operator(input, ">", Gt),
            |input| binary_operator(input, "<", Lt),
        )),
    )(input)?;
    let (input, rhs) = linear_expression(input)?;

    Ok((input, BinaryExpression::new(lhs, op, rhs)))
}

fn variable<T>(input: &str) -> IResult<&str, Variable<T>> {
    let (input, name) = recognize(tuple((
        tag("$"),
        alt((alpha1, tag("_"))),
        many0(alt((alphanumeric1, tag("_")))),
    )))(input)?;

    let var = Variable::new(name);
    Ok((input, var))
}

fn surrounded_by<I, O1, O2, E: ParseError<I>, F, G>(
    outer: F,
    inner: G,
) -> impl Fn(I) -> IResult<I, O2, E>
where
    F: Fn(I) -> IResult<I, O1, E>,
    G: Fn(I) -> IResult<I, O2, E>,
{
    move |input: I| {
        let (input, _) = outer(input)?;
        let (input, result) = inner(input)?;
        let (input, _) = outer(input)?;
        Ok((input, result))
    }
}

fn int(input: &str) -> IResult<&str, Const<BigInt>> {
    let (input, neg) = opt(tag("-"))(input)?;
    let (input, nums) = digit1(input)?;

    let mut result: BigInt = nums.parse().unwrap();
    if neg.is_some() {
        result *= -1;
    }
    Ok((input, result.into()))
}

fn parenthesized<'a, O, F>(inner: F, input: &'a str) -> IResult<&'a str, O>
where
    F: Fn(&'a str) -> IResult<&'a str, O>,
{
    preceded(
        tuple((tag("("), opt(whitespace))),
        terminated(inner, tuple((opt(whitespace), tag(")")))),
    )(input)
}

// TODO: (Security) Ensure a recursion limit
fn linear_expression_leaf(input: &str) -> IResult<&str, LinearExpression> {
    alt((
        |input| parenthesized(linear_expression, input),
        map(int, LinearExpression::Const),
        map(variable, LinearExpression::Variable),
    ))(input)
}

fn any_linear_binary_operator(input: &str) -> IResult<&str, AnyLinearOperator> {
    alt((
        |input| binary_operator(input, "+", Add),
        |input| binary_operator(input, "-", Sub),
        |input| binary_operator(input, "*", Mul),
        |input| binary_operator(input, "/", Div),
    ))(input)
}

fn any_boolean_operator(input: &str) -> IResult<&str, AnyBooleanOp> {
    alt((
        |input| binary_operator(input, "||", Or),
        |input| binary_operator(input, "&&", And),
    ))(input)
}

fn linear_expression(input: &str) -> IResult<&str, LinearExpression> {
    let (input, first) = linear_expression_leaf(input)?;
    let (input, ops) = many0(tuple((
        surrounded_by(whitespace, any_linear_binary_operator),
        linear_expression_leaf,
    )))(input)?;

    fn collapse_tree(
        mut first: LinearExpression,
        rest: Vec<(AnyLinearOperator, LinearExpression)>,
        kind: impl Into<AnyLinearOperator>,
    ) -> (LinearExpression, Vec<(AnyLinearOperator, LinearExpression)>) {
        let mut remain = Vec::new();
        let kind = kind.into();

        for (op, expr) in rest.into_iter() {
            if kind == op {
                let join = move |lhs| {
                    LinearExpression::BinaryExpression(Box::new(BinaryExpression::new(
                        lhs, op, expr,
                    )))
                };
                if let Some((before, last)) = remain.pop() {
                    remain.push((before, join(last)));
                } else {
                    first = join(first)
                }
            } else {
                remain.push((op, expr))
            }
        }

        (first, remain)
    }

    let (first, ops) = collapse_tree(first, ops, Mul);
    let (first, ops) = collapse_tree(first, ops, Div);
    let (first, ops) = collapse_tree(first, ops, Add);
    let (first, ops) = collapse_tree(first, ops, Sub);
    assert_eq!(ops.len(), 0);

    Ok((input, first))
}

fn binary_operator<'a, O>(input: &'a str, tag_: &'_ str, op: impl Into<O>) -> IResult<&'a str, O> {
    let (input, _) = tag(tag_)(input)?;
    Ok((input, op.into()))
}

fn predicate(input: &str) -> IResult<&str, Predicate> {
    let (input, _) = opt(whitespace)(input)?;
    let (input, graphql) = graphql_query(input)?;
    // Whitespace is optional here because graphql_query is greedy and takes it.
    // Shouldn't be a problem though
    let (input, _) = opt(whitespace)(input)?;
    let (input, where_clause) = opt(terminated(where_clause, whitespace))(input)?;
    let (input, _) = opt(whitespace)(input)?;

    let predicate = Predicate {
        graphql,
        where_clause,
    };
    Ok((input, predicate))
}

fn statement(input: &str) -> IResult<&str, Statement> {
    let (input, predicate) = predicate(input)?;
    let (input, _) = tuple((tag("=>"), whitespace))(input)?;
    let (input, cost_expr) = linear_expression(input)?;
    let (input, _) = tag(";")(input)?;
    let (input, _) = opt(whitespace)(input)?;

    let statement = Statement {
        predicate,
        cost_expr,
    };
    Ok((input, statement))
}

pub fn document<'a>(input: &'a str) -> IResult<&'a str, Document<'a>> {
    let (i, statements) = many0(statement)(input)?;
    let document = Document { statements };
    Ok((i, document))
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    fn assert_expr(s: &str, expect: impl Into<BigInt>, v: impl Into<Vars>) {
        let v = v.into();
        let (rest, expr) = linear_expression(s).unwrap();
        assert!(rest.len() == 0);
        let result = expr.eval(&v);
        assert_eq!(Ok(expect.into()), result)
    }

    fn assert_clause(s: &str, expect: bool, v: impl Into<Vars>) {
        let v = v.into();
        let (rest, clause) = where_clause(s).unwrap();
        assert!(rest.len() == 0);
        let result = clause.condition.eval(&v);
        assert_eq!(Ok(expect), result);
    }

    #[test]
    fn binary_expr() {
        assert_expr("1 + 2", 3, ());
    }

    #[test]
    fn operator_precedence() {
        assert_expr("1 + 10 * 2", 21, ());
        assert_expr("10 * 2 + 1", 21, ());
    }

    #[test]
    fn parenthesis() {
        assert_expr("(1 + 10) * 2", 22, ());
    }

    #[test]
    fn where_clauses() {
        assert_clause("where 1 > 2", false, ());
        assert_clause(
            "where $a == $b",
            true,
            (("a", BigInt::from(2)), ("b", BigInt::from(2))),
        );
        assert!(where_clause("where .").is_err());
    }

    // TODO: These operators have precedence in other languages and aren't left to right
    #[test]
    fn left_to_right_booleans() {
        assert_clause("where true || 1 == 0 && false", false, ());
        assert_clause("where 1 == 0 && 1 == 0 || $a", true, ("a", true));
    }

    #[test]
    fn where_parens() {
        assert_clause("where ($a != $a)", false, ("a", BigInt::from(1)));
        assert_clause("where (1 == 0 && 1 == 1) || 1 == 1", true, ());
    }

    #[test]
    fn statements() {
        assert!(statement("query { users(skip: $skip) { tokens } } where 5 == 5 => 1;").is_ok())
    }

    #[test]
    fn doc() {
        // TODO: A test
        let file = "
        query { users(skip: $skip) { tokens } } where $skip > 1000 => 100 + $skip * 10;
        query { users(name: \"Bob\") { tokens } } => 999999; # Bob is evil
        ";

        let _ = document(file);
    }
}