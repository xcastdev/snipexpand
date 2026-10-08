const vscode = require('vscode');
const fs = require('fs');
exports.activate = async function(context) {
    const document = await vscode.workspace.openTextDocument({content: 'READY', language: 'plaintext'});
    await vscode.window.showTextDocument(document);
    function report() {
        const editor = vscode.window.activeTextEditor;
        fs.writeFileSync('/home/vagrant/vscode-state.tmp', JSON.stringify({text:document.getText(),caret:editor && editor.document === document ? document.offsetAt(editor.selection.active) : null}));
        fs.renameSync('/home/vagrant/vscode-state.tmp', '/home/vagrant/vscode-state.json');
    }
    context.subscriptions.push(vscode.workspace.onDidChangeTextDocument(report));
    context.subscriptions.push(vscode.window.onDidChangeTextEditorSelection(report));
    context.subscriptions.push(vscode.commands.registerCommand('snipexpandAcceptance.focus', async () => {
        await vscode.window.showTextDocument(document);
        await vscode.commands.executeCommand('workbench.action.focusActiveEditorGroup');
    }));
    report();
};
