#include "TomBoxImportCommandlet.h"
#include "TomBoxImporter.h"
#include "TomBoxImportOptions.h"
#include "UObject/StrongObjectPtr.h"

DEFINE_LOG_CATEGORY_STATIC(LogTomBoxImportCommandlet, Log, All);

int32 UTomBoxImportCommandlet::Main(const FString& Params)
{
	// Held strongly: saving assets can run garbage collection during the import.
	TStrongObjectPtr<UTomBoxImportOptions> Options(NewObject<UTomBoxImportOptions>());
	FString Value;
	if (FParse::Value(*Params, TEXT("center="), Value, /*bShouldStopOnSeparator=*/false))
	{
		FString X, Y;
		if (!Value.Split(TEXT(","), &X, &Y))
		{
			UE_LOG(LogTomBoxImportCommandlet, Error, TEXT("-center expects X,Y in metres"));
			return 2;
		}
		Options->CenterMeters = FVector2D(FCString::Atod(*X), FCString::Atod(*Y));
	}
	FParse::Value(*Params, TEXT("radius="), Options->RadiusMeters);
	FParse::Value(*Params, TEXT("level="), Options->DetailLevel);
	FParse::Value(*Params, TEXT("server="), Options->ServerUrl);
	FParse::Value(*Params, TEXT("content="), Options->ContentPath);
	FParse::Value(*Params, TEXT("map="), Options->MapName);
	Options->bImportTextures = !FParse::Param(*Params, TEXT("notextures"));
	Options->bNanite = FParse::Param(*Params, TEXT("nanite"));
	FParse::Value(*Params, TEXT("surroundings="), Options->SurroundingsMeters);
	FParse::Value(*Params, TEXT("maxtex="), Options->MaxTextureSize);
	Options->bNormalMaps = !FParse::Param(*Params, TEXT("nonormals"));
	Options->bFlipNormalGreen = FParse::Param(*Params, TEXT("flipgreen"));
	Options->bLumenVolume = !FParse::Param(*Params, TEXT("nolumen"));

	FTomBoxImporter Importer(*Options);
	FTomBoxImportReport Report;
	const bool bOk = Importer.Run(Report);
	UE_LOG(LogTomBoxImportCommandlet, Display, TEXT("TomBox import %s: %s"), bOk ? TEXT("done") : TEXT("FAILED"), *Report.ToString());
	return bOk ? 0 : 1;
}
