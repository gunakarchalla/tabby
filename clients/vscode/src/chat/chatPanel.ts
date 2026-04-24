import { window, ExtensionContext, ViewColumn } from "vscode";
import { ChatWebview } from "./webview";
import type { Client } from "../lsp/client";
import type { Config } from "../Config";
import type { GitProvider } from "../git/GitProvider";

export async function createChatPanel(context: ExtensionContext, client: Client, gitProvider: GitProvider, config?: Config) {
  const panel = window.createWebviewPanel(`tabby.chat.panel`, "Tabby", ViewColumn.One, {
    retainContextWhenHidden: true,
  });

  const chatWebview = new ChatWebview(context, client, gitProvider, config);
  chatWebview.init(panel.webview);

  panel.onDidDispose(() => {
    chatWebview.dispose();
  });
}
