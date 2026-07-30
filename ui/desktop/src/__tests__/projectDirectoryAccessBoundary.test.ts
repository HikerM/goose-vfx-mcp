import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

const mainSource = readFileSync(resolve(__dirname, '../main.ts'), 'utf8');
const mentionPopoverSource = readFileSync(resolve(__dirname, '../components/MentionPopover.tsx'), 'utf8');
const mainFile = ts.createSourceFile('main.ts', mainSource, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const mentionPopoverFile = ts.createSourceFile('MentionPopover.tsx', mentionPopoverSource, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);

function stringValue(node: ts.Expression): string | undefined {
  return ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) ? node.text : undefined;
}

function findHandler(channel: string): ts.ArrowFunction | ts.FunctionExpression {
  const handlers: (ts.ArrowFunction | ts.FunctionExpression)[] = [];
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression) &&
      ts.isIdentifier(node.expression.expression) && node.expression.expression.text === 'ipcMain' &&
      node.expression.name.text === 'handle' && node.arguments.length >= 2 &&
      ts.isStringLiteral(node.arguments[0]) && node.arguments[0].text === channel) {
      const callback = node.arguments[1];
      if (ts.isArrowFunction(callback) || ts.isFunctionExpression(callback)) handlers.push(callback);
    }
    ts.forEachChild(node, visit);
  };
  visit(mainFile);
  expect(handlers, `expected exactly one IPC handler: ${channel}`).toHaveLength(1);
  return handlers[0];
}

function statementsOf(handler: ts.ArrowFunction | ts.FunctionExpression): readonly ts.Statement[] {
  return ts.isBlock(handler.body) ? handler.body.statements : [];
}

function isProcessPlatformWin32(node: ts.Expression): boolean {
  return ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsEqualsEqualsToken &&
    ts.isPropertyAccessExpression(node.left) && ts.isIdentifier(node.left.expression) &&
    node.left.expression.text === 'process' && node.left.name.text === 'platform' && stringValue(node.right) === 'win32';
}

function hasForbiddenCall(node: ts.Node, names: ReadonlySet<string>): boolean {
  let found = false;
  const visit = (current: ts.Node): void => {
    if (ts.isCallExpression(current) && ts.isIdentifier(current.expression) && names.has(current.expression.text)) found = true;
    ts.forEachChild(current, visit);
  };
  visit(node);
  return found;
}

describe('project directory access boundaries', () => {
  it('rejects Windows project access before trust, path handling, or dialogs', () => {
    const statements = statementsOf(findHandler('request-project-directory-access')).filter((statement) => !ts.isEmptyStatement(statement));
    expect(ts.isIfStatement(statements[0])).toBe(true);
    if (!ts.isIfStatement(statements[0])) return;
    expect(isProcessPlatformWin32(statements[0].expression)).toBe(true);
    expect(statements[0].elseStatement).toBeUndefined();
    const branch = ts.isBlock(statements[0].thenStatement) ? statements[0].thenStatement.statements : [statements[0].thenStatement];
    expect(branch).toHaveLength(1);
    expect(ts.isThrowStatement(branch[0])).toBe(true);
  });

  it.each(['list-project-files', 'list-git-worktree-dirs'])('fails closed for missing, invalid, and arbitrary inputs: %s', (channel) => {
    const statements = statementsOf(findHandler(channel)).filter((statement) => !ts.isEmptyStatement(statement));
    expect(statements).toHaveLength(1);
    expect(ts.isReturnStatement(statements[0])).toBe(true);
    if (!ts.isReturnStatement(statements[0]) || !statements[0].expression || !ts.isArrayLiteralExpression(statements[0].expression)) return;
    expect(statements[0].expression.elements).toHaveLength(0);
  });

  it('does not request project authorization or enumerate files from MentionPopover', () => {
    const forbidden = new Set(['requestProjectDirectoryAccess', 'directory-chooser', 'list-project-files', 'listGitWorktreeDirs', 'listProjectFiles']);
    const aliases = new Set(forbidden);
    const visit = (node: ts.Node): void => {
      if (ts.isImportDeclaration(node) && node.importClause?.namedBindings && ts.isNamedImports(node.importClause.namedBindings)) {
        for (const specifier of node.importClause.namedBindings.elements) {
          const imported = specifier.propertyName?.text ?? specifier.name.text;
          if (forbidden.has(imported)) aliases.add(specifier.name.text);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(mentionPopoverFile);
    expect(hasForbiddenCall(mentionPopoverFile, aliases)).toBe(false);
  });
});
