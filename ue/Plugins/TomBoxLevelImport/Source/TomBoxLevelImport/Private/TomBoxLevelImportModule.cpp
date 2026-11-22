// Editor menu: Tools > TomBox: Import Map Area... opens a window with the import options and
// an Import button; the created map is opened when the import finishes.

#include "TomBoxImporter.h"
#include "TomBoxImportOptions.h"

#include "Modules/ModuleManager.h"
#include "ToolMenus.h"
#include "PropertyEditorModule.h"
#include "IDetailsView.h"
#include "Widgets/SWindow.h"
#include "Widgets/Layout/SBox.h"
#include "Widgets/Input/SButton.h"
#include "Widgets/Text/STextBlock.h"
#include "Widgets/SBoxPanel.h"
#include "Framework/Application/SlateApplication.h"
#include "Misc/MessageDialog.h"
#include "FileHelpers.h"
#include "Misc/PackageName.h"
#include "UObject/StrongObjectPtr.h"

#define LOCTEXT_NAMESPACE "TomBoxLevelImport"

class FTomBoxLevelImportModule : public IModuleInterface
{
public:
	virtual void StartupModule() override
	{
		UToolMenus::RegisterStartupCallback(FSimpleMulticastDelegate::FDelegate::CreateRaw(this, &FTomBoxLevelImportModule::RegisterMenus));
	}

	virtual void ShutdownModule() override
	{
		UToolMenus::UnRegisterStartupCallback(this);
		UToolMenus::UnregisterOwner(this);
		Options.Reset();
	}

private:
	void RegisterMenus()
	{
		FToolMenuOwnerScoped Owner(this);
		UToolMenu* Menu = UToolMenus::Get()->ExtendMenu(TEXT("LevelEditor.MainMenu.Tools"));
		FToolMenuSection& Section = Menu->FindOrAddSection(TEXT("TomBox"), LOCTEXT("Section", "TomBox"));
		Section.AddMenuEntry(TEXT("TomBoxImportArea"), LOCTEXT("Entry", "TomBox: Import Map Area..."),
			LOCTEXT("EntryTip", "Import an area of the level from a running TomBox map server as meshes, textures and a map."),
			FSlateIcon(), FUIAction(FExecuteAction::CreateRaw(this, &FTomBoxLevelImportModule::OpenWindow)));
	}

	void OpenWindow()
	{
		if (!Options.IsValid())
		{
			UTomBoxImportOptions* O = NewObject<UTomBoxImportOptions>();
			O->LoadConfig();
			Options.Reset(O);
		}
		FPropertyEditorModule& PropertyEditor = FModuleManager::LoadModuleChecked<FPropertyEditorModule>(TEXT("PropertyEditor"));
		FDetailsViewArgs Args;
		Args.bAllowSearch = false;
		Args.NameAreaSettings = FDetailsViewArgs::HideNameArea;
		TSharedRef<IDetailsView> Details = PropertyEditor.CreateDetailView(Args);
		Details->SetObject(Options.Get());

		TSharedRef<SWindow> Window = SNew(SWindow)
			.Title(LOCTEXT("Title", "TomBox: Import Map Area"))
			.ClientSize(FVector2D(520, 420));
		TWeakPtr<SWindow> WeakWindow = Window;
		Window->SetContent(
			SNew(SVerticalBox)
			+ SVerticalBox::Slot().FillHeight(1.f)[Details]
			+ SVerticalBox::Slot().AutoHeight().Padding(8)
			[
				SNew(STextBlock).AutoWrapText(true).Text(LOCTEXT("Hint",
					"Start the map server first (tools/rage/mapview/server.py). Coordinates are the game's, in metres, as the web viewer shows them."))
			]
			+ SVerticalBox::Slot().AutoHeight().HAlign(HAlign_Right).Padding(8)
			[
				SNew(SButton).Text(LOCTEXT("Import", "Import")).OnClicked_Lambda([this, WeakWindow]()
				{
					if (TSharedPtr<SWindow> W = WeakWindow.Pin())
					{
						W->RequestDestroyWindow();
					}
					RunImport();
					return FReply::Handled();
				})
			]);
		FSlateApplication::Get().AddWindow(Window);
	}

	void RunImport()
	{
		Options->SaveConfig();
		FTomBoxImporter Importer(*Options);
		FTomBoxImportReport Report;
		const bool bOk = Importer.Run(Report);
		UE_LOG(LogTemp, Display, TEXT("TomBox import: %s"), *Report.ToString());
		FMessageDialog::Open(EAppMsgType::Ok, FText::FromString(
			(bOk ? FString(TEXT("Import finished.\n\n")) : FString(TEXT("Import failed.\n\n"))) + Report.ToString()));
		if (bOk && !Report.MapPackage.IsEmpty())
		{
			FEditorFileUtils::LoadMap(FPackageName::LongPackageNameToFilename(Report.MapPackage, FPackageName::GetMapPackageExtension()));
		}
	}

	TStrongObjectPtr<UTomBoxImportOptions> Options;
};

IMPLEMENT_MODULE(FTomBoxLevelImportModule, TomBoxLevelImport)

#undef LOCTEXT_NAMESPACE
